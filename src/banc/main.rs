//! `cs-banc` : la session seule porte-t-elle assez de contexte pour décider ?
//! Compare des modèles au tri de l'utilisateur, à partir des instantanés figés de `cs review`.
mod adapter;
#[path = "../categories.rs"]
mod categories;
mod claude_cli;
mod dataset;
mod http;
mod mock;
mod runner;
mod score;
mod synth;

use adapter::Classifier;
use anyhow::{bail, Result};
use clap::{Parser, Subcommand};
use std::collections::HashMap;
use std::path::PathBuf;

const QWEN_DEFAULT_URL: &str = "http://localhost:1234";
/// Vérifiés le 07/11 auprès de l'API, avec du texte synthétique.
const JEV_URL: &str = "https://api.typesafe.ai";
const JEV_MODEL: &str = "jev-latest";
const JEV_KEY_SERVICE: &str = "cs-cli-utility-transcript-screening-jev-api-key";

#[derive(Parser)]
#[command(name = "cs-banc", about = "Compare des modèles au tri de cs review")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Écrire un jeu synthétique
    Synth {
        #[arg(long, default_value_t = 1)]
        seed: u64,
        #[arg(long, default_value_t = 83)]
        n: usize,
        #[arg(long)]
        out: PathBuf,
    },
    /// Interroger des modèles et écrire le rapport
    Run {
        #[arg(long)]
        dataset: PathBuf,
        /// mock, qwen, jev, clef-flash, clef, sonnet, opus (séparés par des virgules)
        #[arg(long, value_delimiter = ',')]
        models: Vec<String>,
        #[arg(long)]
        out: PathBuf,
        /// Adresse d'un modèle : clef=URL et clef-flash=URL sont requis, qwen et jev ont un défaut
        #[arg(long = "endpoint", value_name = "MODELE=URL")]
        endpoints: Vec<String>,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// Afficher ce qui serait envoyé et où, sans rien envoyer ni écrire
        #[arg(long)]
        dry_run: bool,
    },
}

/// Les modèles qui ont une adresse réglable. Une faute de frappe ne doit pas retomber
/// en silence sur une adresse par défaut, et Jev garde la sienne : la clé y est liée.
const SETTABLE_ENDPOINTS: [&str; 3] = ["qwen", "clef", "clef-flash"];

fn parse_endpoints(raw: &[String]) -> Result<HashMap<String, String>> {
    raw.iter()
        .map(|entry| {
            let Some((model, url)) = entry.split_once('=') else {
                bail!("--endpoint attend MODELE=URL : {entry}");
            };
            if !SETTABLE_ENDPOINTS.contains(&model) {
                bail!("--endpoint : modèle non réglable : {model}");
            }
            if let Err(reason) = adapter::checked_url(url) {
                bail!("--endpoint {model} : {reason}");
            }
            Ok((model.to_string(), url.to_string()))
        })
        .collect()
}

/// Construit un modèle sans lire aucune clé : elle ne l'est qu'au premier envoi.
fn build(name: &str, endpoints: &HashMap<String, String>) -> Result<Box<dyn Classifier>> {
    let endpoint = |model: &str| {
        endpoints
            .get(model)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("--endpoint {model}=URL est requis"))
    };
    Ok(match name {
        "qwen" => {
            let url = endpoints.get("qwen").map_or(QWEN_DEFAULT_URL, String::as_str);
            Box::new(http::LmStudio::qwen(url))
        }
        "jev" => Box::new(http::SystemOne::new(
            "jev",
            JEV_URL,
            JEV_MODEL,
            http::ApiKey::Keychain(JEV_KEY_SERVICE.to_string()),
        )),
        // Clef tourne là où on le lance : l'adaptateur ne connaît que l'adresse.
        "clef" | "clef-flash" => Box::new(http::SystemOne::new(name, &endpoint(name)?, name, http::ApiKey::None)),
        "sonnet" => Box::new(claude_cli::ClaudeCli::sonnet()),
        "opus" => Box::new(claude_cli::ClaudeCli::opus()),
        other => bail!("modèle inconnu : {other}"),
    })
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Synth { seed, n, out } => {
            synth::dataset(seed, n, &out)?;
            println!("✓ {n} cas synthétiques dans {}", out.display());
        }
        Command::Run { dataset, models, out, endpoints, seed, dry_run } => {
            let endpoints = parse_endpoints(&endpoints)?;
            let distinct: std::collections::HashSet<&String> = models.iter().collect();
            if distinct.len() != models.len() {
                bail!("--models : modèle en double");
            }
            let data = dataset::load(&dataset).map_err(anyhow::Error::msg)?;
            let mut classifiers = Vec::new();
            for name in &models {
                classifiers.push(if name == "mock" {
                    Box::new(mock::Mock::new(0.7, seed, &data.cases)) as Box<dyn Classifier>
                } else {
                    build(name, &endpoints)?
                });
            }
            if dry_run {
                print!("{}", runner::dry_run(&data, &classifiers, &dataset, &out));
                return Ok(());
            }
            let reports = runner::run(&data, &classifiers, &out, seed).map_err(anyhow::Error::msg)?;
            let path = out.join("report.md");
            runner::write_report(&path, &runner::render(&data, &reports)).map_err(anyhow::Error::msg)?;
            println!("✓ rapport : {}", path.display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(raw: &[&str]) -> Vec<String> {
        raw.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn malformed_endpoint_is_refused() {
        assert!(parse_endpoints(&entries(&["clef"])).is_err());
        assert!(parse_endpoints(&entries(&["clfe=http://a"])).is_err());
        assert!(parse_endpoints(&entries(&["clef=ftp://a"])).is_err());
        assert!(parse_endpoints(&entries(&["clef=http://127.0.0.1:1\\\\@machine"])).is_err());
        assert!(parse_endpoints(&entries(&["clef=http://machine:8000"])).is_ok());
    }

    #[test]
    fn jev_address_cannot_be_changed() {
        assert!(parse_endpoints(&entries(&["jev=http://127.0.0.1:1"])).is_err());
        let endpoints = HashMap::new();
        let jev = build("jev", &endpoints).unwrap();
        assert_eq!(jev.target(), "api.typesafe.ai");
    }

    #[test]
    fn clef_requires_an_address_and_sends_to_it() {
        assert!(build("clef", &HashMap::new()).is_err());
        let endpoints = parse_endpoints(&entries(&["clef=https://api.typesafe.ai"])).unwrap();
        assert_eq!(build("clef", &endpoints).unwrap().target(), "api.typesafe.ai");
    }
}
