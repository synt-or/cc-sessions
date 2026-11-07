//! Fait tourner les modèles sur un jeu, avec reprise, et écrit le rapport.
use crate::adapter::{probabilities_from, validate, Classifier, Prediction};
use crate::dataset::{Case, Dataset};
use crate::score::{advantage, score, split_by_flag, Advantage, Outcome, Report};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::Path;

/// Au-delà, un service est considéré en panne : inutile de lui soumettre le reste.
const MAX_CONSECUTIVE_FAILURES: usize = 5;
const STOPPED: &str = "non interrogé : pannes en série";
/// Au-delà de cette part d'échecs, les scores ne mesurent plus le modèle.
const MAX_FAILURE_SHARE_FOR_VERDICT: f64 = 0.1;
const PROGRESS_EVERY: usize = 10;
const UNKNOWN_VERSION: &str = "(non annoncée)";

fn short_signature(model: &dyn Classifier) -> String {
    let digest = Sha256::digest(model.signature().as_bytes());
    format!("{digest:x}")[..16].to_string()
}

fn key(signature: &str, case: &Case) -> String {
    format!("{signature}\t{}\t{}", case.session_id, case.snapshot_hash)
}

/// Les réponses valides déjà écrites. Un échec n'est pas repris : une panne passagère
/// ne doit pas devenir une erreur définitive du modèle.
fn load_previous(path: &Path) -> HashMap<String, Prediction> {
    let mut out = HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return out;
    };
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let (Some(signature), Some(sid), Some(hash)) =
            (v["signature"].as_str(), v["sessionId"].as_str(), v["snapshotHash"].as_str())
        else {
            continue;
        };
        if !v["error"].is_null() {
            continue;
        }
        let parsed = probabilities_from(&v["probabilities"])
            .and_then(|p| validate(p, v["latencyMs"].as_u64().unwrap_or(0)));
        if let Ok(mut prediction) = parsed {
            prediction.model_version = v["modelVersion"].as_str().map(String::from);
            out.insert(format!("{signature}\t{sid}\t{hash}"), prediction);
        }
    }
    out
}

/// Décrit ce que `run` enverrait, et où, sans rien envoyer ni écrire. `dir` : le jeu.
pub fn dry_run(data: &Dataset, models: &[Box<dyn Classifier>], dir: &Path, out: &Path) -> String {
    let previous = load_previous(&out.join("predictions.jsonl"));
    let mut s = String::from("Essai à blanc : rien n'est envoyé.\n");
    if data.synthetic {
        s.push_str("Données synthétiques.\n");
    } else {
        s.push_str("**Données réelles : le texte ci-dessous vient de vraies sessions.**\n");
    }
    for model in models {
        let signature = short_signature(model.as_ref());
        let to_send: Vec<&Case> = data
            .cases
            .iter()
            .filter(|c| !previous.contains_key(&key(&signature, c)))
            .collect();
        let bytes: usize = to_send.iter().map(|c| c.text.len()).sum();
        s.push_str(&format!(
            "\n## {} → {}\n\n{} cas à envoyer sur {}, {} octets de texte d'instantané.\n\nAvec chaque cas : {}\n\nFichiers envoyés tels quels :\n",
            model.name(),
            model.target(),
            to_send.len(),
            data.cases.len(),
            bytes,
            model.preamble(),
        ));
        for case in &to_send {
            let path = dir.join("snapshots").join(&case.session_id).join(format!("{}.txt", case.snapshot_hash));
            s.push_str(&format!("- {} ({} octets)\n", path.display(), case.text.len()));
        }
    }
    if let Some(first) = data.cases.first() {
        s.push_str(&format!("\n## Texte exact du premier cas ({})\n\n{}", first.session_id, first.text));
    }
    s
}

pub struct ModelReport {
    pub name: String,
    pub versions: BTreeSet<String>,
    pub failure_kinds: BTreeMap<String, usize>,
    pub all: Report,
    pub with_flag: Report,
    pub without_flag: Report,
    /// Sur les cas sans bandeau, où seul le contenu de la session peut aider.
    pub advantage: Option<Advantage>,
}

/// Refuse un chemin sous un dépôt git, liens et chemins relatifs compris. Ne change les
/// droits que d'un dossier créé ici : un dossier existant reste tel qu'il est.
fn private_dir(out: &Path) -> Result<(), String> {
    let real = crate::dataset::resolved(out).ok_or("dossier de sortie : .. ou lien pendant, refus")?;
    if crate::dataset::is_real_state(out) {
        return Err("le dossier d'état réel n'accueille pas les résultats du banc : refus".into());
    }
    if real.ancestors().any(|dir| dir.join(".git").exists()) {
        return Err(format!("{} est dans un dépôt git : refus", out.display()));
    }
    if out.exists() {
        return Ok(());
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(out)
        .map_err(|e| format!("dossier de sortie : {}", e.kind()))
}

/// Une panne du service, contrairement à un refus propre à ce cas (une entrée trop
/// longue, par exemple), qui n'empêche pas d'interroger les suivants.
fn is_outage(error: &str) -> bool {
    error.starts_with("transport")
        || error.starts_with("délai")
        || error.starts_with("lancement")
        || error.starts_with("trousseau")
        || error.starts_with("clé")
        || error.starts_with("réponse HTTP 5")
        || error.starts_with("réponse HTTP 401")
        || error.starts_with("réponse HTTP 403")
        || error.starts_with("réponse HTTP 429")
}

/// Interroge chaque modèle sur chaque cas, en reprenant là où un essai s'est arrêté.
pub fn run(
    data: &Dataset,
    models: &[Box<dyn Classifier>],
    out: &Path,
    seed: u64,
) -> Result<Vec<ModelReport>, String> {
    if models.is_empty() {
        return Err("aucun modèle demandé".into());
    }
    if data.cases.is_empty() {
        return Err("aucun cas à évaluer".into());
    }
    private_dir(out)?;
    let path = out.join("predictions.jsonl");
    // Un lien ferait écrire ailleurs, et jusque dans le vrai tri.
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink() || m.nlink() > 1)
    {
        return Err("predictions.jsonl est un lien : refus".into());
    }
    let previous = load_previous(&path);
    // Une coupure peut laisser une ligne incomplète : la suivante ne doit pas s'y coller.
    let truncated = std::fs::read(&path).ok().is_some_and(|b| b.last().is_some_and(|c| *c != b'\n'));
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .map_err(|e| format!("predictions.jsonl : {}", e.kind()))?;
    if truncated {
        file.write_all(b"\n").map_err(|e| format!("predictions.jsonl : {}", e.kind()))?;
    }

    let mut reports = Vec::new();
    for model in models {
        let signature = short_signature(model.as_ref());
        let mut outcomes = Vec::new();
        let mut consecutive = 0;
        let mut versions = BTreeSet::new();
        for (index, case) in data.cases.iter().enumerate() {
            if index % PROGRESS_EVERY == 0 {
                eprintln!("{} : {index}/{}", model.name(), data.cases.len());
            }
            let result = match previous.get(&key(&signature, case)) {
                Some(done) => Ok(done.clone()),
                None if consecutive >= MAX_CONSECUTIVE_FAILURES => Err(STOPPED.to_string()),
                None => {
                    let result = model.predict(&case.text);
                    consecutive = match &result {
                        Err(error) if is_outage(error) => consecutive + 1,
                        _ => 0,
                    };
                    let line = match &result {
                        Ok(p) => json!({
                            "model": model.name(), "target": model.target(), "signature": signature,
                            "sessionId": case.session_id, "snapshotHash": case.snapshot_hash,
                            "probabilities": p.probabilities, "latencyMs": p.latency_ms,
                            "modelVersion": p.model_version,
                        }),
                        Err(error) => json!({
                            "model": model.name(), "target": model.target(), "signature": signature,
                            "sessionId": case.session_id, "snapshotHash": case.snapshot_hash,
                            "error": error,
                        }),
                    };
                    file.write_all(format!("{line}\n").as_bytes())
                        .map_err(|e| format!("predictions.jsonl : {}", e.kind()))?;
                    result
                }
            };
            if let Ok(p) = &result {
                versions.insert(p.model_version.clone().unwrap_or_else(|| UNKNOWN_VERSION.to_string()));
                // Tout de suite : les appels suivants seraient payés pour rien.
                if versions.len() > 1 {
                    return Err(format!(
                        "versions mêlées pour {} ({}) : utiliser un autre --out",
                        model.name(),
                        versions.iter().cloned().collect::<Vec<_>>().join(", ")
                    ));
                }
            }
            outcomes.push(Outcome {
                truth: case.truth.clone(),
                has_prior_flag: case.has_prior_flag,
                prior_status: case.prior_status.clone(),
                result,
            });
        }
        versions.remove(UNKNOWN_VERSION);
        let mut failure_kinds = BTreeMap::new();
        for o in &outcomes {
            if let Err(error) = &o.result {
                *failure_kinds.entry(error.clone()).or_default() += 1;
            }
        }
        let (with, without) = split_by_flag(&outcomes);
        reports.push(ModelReport {
            name: model.name().to_string(),
            versions,
            failure_kinds,
            all: score(&outcomes, seed),
            with_flag: score(&with, seed),
            without_flag: score(&without, seed),
            advantage: advantage(&without, models.len(), seed),
        });
    }
    Ok(reports)
}

fn pct(x: f64) -> String {
    format!("{:.1} %", x * 100.0)
}

fn table_line(name: &str, r: &Report) -> String {
    format!(
        "| {name} | {} | {} | {} à {} | {} | {} | {} | {:.3} | {:.3} | {:.3} | {} | {} ms | {} ms |\n",
        r.n,
        pct(r.accuracy),
        pct(r.accuracy_ci95.0),
        pct(r.accuracy_ci95.1),
        pct(r.baseline_accuracy),
        pct(r.macro_f1),
        pct(r.top2),
        r.log_loss,
        r.brier,
        r.kappa,
        r.failures,
        r.latency_p50_ms,
        r.latency_p95_ms,
    )
}

const TABLE_HEAD: &str = "| Modèle | Cas | Justesse | IC 95 % | Référence | F1 macro | Top 2 | Log loss | Brier | Kappa | Échecs | Latence médiane | Latence p95 |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|\n";

/// Le rapport commence par la question de l'utilisateur : l'instantané suffit-il ? La réponse
/// porte sur les cas sans bandeau, où seul le contenu de la session peut aider.
pub fn render(data: &Dataset, reports: &[ModelReport]) -> String {
    let mut s = String::from("# Banc de classifieurs\n\n");
    if data.synthetic {
        s.push_str("**Données synthétiques : ces chiffres ne disent rien des vraies sessions.**\n\n");
    }
    let best = reports
        .iter()
        .filter(|m| m.advantage.is_some())
        .max_by(|a, b| a.without_flag.accuracy.total_cmp(&b.without_flag.accuracy));
    let unmeasured = reports.iter().find(|m| {
        let r = &m.without_flag;
        r.n > 0 && r.failures as f64 / r.n as f64 > MAX_FAILURE_SHARE_FOR_VERDICT
    });
    match best {
        // Une panne ne dit rien de la session : pas de conclusion sur ce qui n'est pas mesuré.
        Some(_) if unmeasured.is_some() => {
            let m = unmeasured.expect("vérifié ci-dessus");
            s.push_str(&format!(
                "**Pas de verdict : {} a échoué sur {} des {} cas sans bandeau.** Relancer une fois le service rétabli.\n\n",
                m.name, m.without_flag.failures, m.without_flag.n,
            ));
        }
        Some(m) => {
            let a = m.advantage.expect("filtré ci-dessus");
            let verdict = if a.beats_baseline() {
                "il bat la référence naïve, au-delà de l'incertitude"
            } else {
                "rien ne démontre qu'il bat la référence naïve, compte tenu de l'incertitude"
            };
            s.push_str(&format!(
                "Sur les {} cas sans bandeau, meilleur modèle : **{}**, {} de justesse contre {} en répondant toujours la catégorie majoritaire. Écart de {} points (intervalle à {:.1} % corrigé pour {} modèles : {:+.1} à {:+.1}) : {verdict}.\n\n",
                m.without_flag.n,
                m.name,
                pct(m.without_flag.accuracy),
                pct(m.without_flag.baseline_accuracy),
                format_args!("{:+.1}", a.difference * 100.0),
                a.confidence * 100.0,
                reports.len(),
                a.low * 100.0,
                a.high * 100.0,
            ));
        }
        None => s.push_str("Aucun cas sans bandeau : pas de verdict.\n\n"),
    }
    let skipped = &data.skipped;
    s.push_str(&format!(
        "{} cas. Écartés : {} sans instantané, {} au hash faux, {} de catégorie inconnue, {} à l'identifiant invalide, {} au transcript illisible. Lignes d'historique illisibles : {}.\n\n",
        data.cases.len(),
        skipped.missing_snapshot,
        skipped.bad_hash,
        skipped.unknown_category,
        skipped.bad_id,
        skipped.read_error,
        skipped.unreadable_lines,
    ));
    s.push_str("## Sans les sessions au bandeau de flag\n\nLe bandeau donne un indice sur le tri passé : ce tableau mesure ce que porte le reste de la session.\n\n");
    s.push_str(TABLE_HEAD);
    for m in reports {
        s.push_str(&table_line(&m.name, &m.without_flag));
    }
    s.push_str("\n## Sessions au bandeau de flag\n\n");
    if let Some(copy) = reports.first().and_then(|m| m.with_flag.copy_flag_accuracy) {
        s.push_str(&format!("Reprendre simplement le statut du bandeau atteint {}.\n\n", pct(copy)));
    }
    s.push_str(TABLE_HEAD);
    for m in reports {
        s.push_str(&table_line(&m.name, &m.with_flag));
    }
    s.push_str("\n## Tous les cas\n\n");
    s.push_str(TABLE_HEAD);
    for m in reports {
        s.push_str(&table_line(&m.name, &m.all));
    }
    for m in reports {
        s.push_str(&format!("\n## {}\n\n", m.name));
        if !m.versions.is_empty() {
            let versions: Vec<&str> = m.versions.iter().map(String::as_str).collect();
            s.push_str(&format!("Version annoncée : {}.\n\n", versions.join(", ")));
        }
        if !m.failure_kinds.is_empty() {
            s.push_str("Échecs :\n\n");
            for (kind, count) in &m.failure_kinds {
                s.push_str(&format!("- {kind} : {count}\n"));
            }
            s.push('\n');
        }
        s.push_str("Rappel par catégorie :\n\n");
        for (category, recall) in &m.all.recall_by_category {
            s.push_str(&format!("- {category} : {}\n", pct(*recall)));
        }
        s.push_str("\nConfusion (vérité → prédiction) :\n\n");
        for (truth, row) in &m.all.confusion {
            let cells: Vec<String> = row.iter().map(|(p, n)| format!("{p} {n}")).collect();
            s.push_str(&format!("- {truth} → {}\n", cells.join(", ")));
        }
        s.push_str("\nCalibration (confiance annoncée, justesse observée) :\n\n");
        for bin in &m.all.calibration {
            s.push_str(&format!(
                "- {:.0} à {:.0} % : {} cas, confiance {}, justesse {}\n",
                bin.low * 100.0,
                bin.high * 100.0,
                bin.n,
                pct(bin.mean_confidence),
                pct(bin.accuracy),
            ));
        }
    }
    s
}

/// Écrit le rapport, lisible par l'utilisateur seul. Un nouveau fichier remplace l'ancien :
/// si ce nom était un lien, seul le lien disparaît, sa cible reste intacte.
pub fn write_report(path: &Path, text: &str) -> Result<(), String> {
    let error = |e: std::io::Error| format!("rapport : {}", e.kind());
    let tmp = path.with_extension(format!("md.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.map_err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::tests::uniform;
    use crate::dataset::load;
    use std::os::unix::fs::PermissionsExt;
    use crate::mock::Mock;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    struct Counting {
        calls: Rc<Cell<u32>>,
        signature: String,
        version: Option<String>,
        fail: Rc<Cell<bool>>,
        answer: Option<String>,
    }

    impl Counting {
        fn new(calls: Rc<Cell<u32>>) -> Counting {
            Counting {
                calls,
                signature: "sig".into(),
                version: None,
                fail: Rc::new(Cell::new(false)),
                answer: None,
            }
        }
    }

    impl Classifier for Counting {
        fn name(&self) -> &str {
            "compteur"
        }
        fn target(&self) -> String {
            "hote".into()
        }
        fn preamble(&self) -> String {
            "consignes de test".into()
        }
        fn signature(&self) -> String {
            self.signature.clone()
        }
        fn predict(&self, _text: &str) -> Result<Prediction, String> {
            self.calls.set(self.calls.get() + 1);
            if self.fail.get() {
                return Err("transport : panne".into());
            }
            let mut raw = uniform();
            if let Some(answer) = &self.answer {
                for (k, v) in raw.iter_mut() {
                    *v = if k == answer { 0.6 } else { 0.1 };
                }
            }
            let mut p = validate(raw, 5)?;
            p.model_version = self.version.clone();
            Ok(p)
        }
    }

    fn synthetic(name: &str, n: usize) -> (Dataset, std::path::PathBuf) {
        let d = std::env::temp_dir().join(format!("cs_banc_run_{name}"));
        std::fs::remove_dir_all(&d).ok();
        crate::synth::dataset(1, n, &d).unwrap();
        (load(&d).unwrap(), d)
    }

    #[test]
    fn end_to_end_on_synthetic_with_mock() {
        let (data, dir) = synthetic("e2e", 83);
        let models: Vec<Box<dyn Classifier>> = vec![Box::new(Mock::new(0.8, 1, &data.cases))];
        let reports = run(&data, &models, &dir.join("out"), 1).unwrap();
        let text = render(&data, &reports);
        for part in ["Données synthétiques", "**mock**", "cas sans bandeau", "Référence", "Sans les sessions au bandeau", "Latence p95", "Reprendre simplement"] {
            assert!(text.contains(part), "{part} :\n{text}");
        }
        assert_eq!(reports[0].all.n, 83);
        assert_eq!(reports[0].with_flag.n + reports[0].without_flag.n, 83);
    }

    #[test]
    fn dry_run_shows_content_and_destination_without_sending() {
        let (data, dir) = synthetic("dry", 4);
        let calls = Rc::new(Cell::new(0));
        let models: Vec<Box<dyn Classifier>> = vec![Box::new(Counting::new(calls.clone()))];
        let out = dir.join("out-absent");
        let text = dry_run(&data, &models, &dir, &out);
        assert_eq!(calls.get(), 0, "aucun envoi");
        assert!(!out.exists(), "aucune écriture");
        for part in ["Essai à blanc", "compteur → hote", "4 cas à envoyer sur 4", "consignes de test", "cs-review-snapshot v1"] {
            assert!(text.contains(part), "{part} :\n{text}");
        }
        assert!(text.contains(&format!("{}.txt", data.cases[0].snapshot_hash)));
    }

    #[test]
    fn dry_run_leaves_out_what_was_already_answered() {
        let (data, dir) = synthetic("dry_resume", 3);
        let calls = Rc::new(Cell::new(0));
        let models: Vec<Box<dyn Classifier>> = vec![Box::new(Counting::new(calls.clone()))];
        run(&data, &models, &dir.join("out"), 1).unwrap();
        assert!(dry_run(&data, &models, &dir, &dir.join("out")).contains("0 cas à envoyer sur 3"));
        assert_eq!(calls.get(), 3);
    }

    #[test]
    fn dry_run_says_when_data_is_real() {
        let (mut data, dir) = synthetic("dry_real", 1);
        data.synthetic = false;
        let models: Vec<Box<dyn Classifier>> = vec![Box::new(Mock::new(1.0, 1, &data.cases))];
        assert!(dry_run(&data, &models, &dir, &dir.join("o")).contains("Données réelles"));
    }

    #[test]
    fn predictions_file_never_contains_snapshot_text() {
        let (data, dir) = synthetic("private", 5);
        let models: Vec<Box<dyn Classifier>> = vec![Box::new(Mock::new(0.8, 1, &data.cases))];
        run(&data, &models, &dir.join("out"), 1).unwrap();
        let written = std::fs::read_to_string(dir.join("out/predictions.jsonl")).unwrap();
        assert!(!written.contains("chantier fictif"), "{written}");
        let mode = std::fs::metadata(dir.join("out/predictions.jsonl")).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn resume_skips_answers_already_written() {
        let (data, dir) = synthetic("resume", 6);
        let calls = Rc::new(Cell::new(0));
        let models: Vec<Box<dyn Classifier>> = vec![Box::new(Counting::new(calls.clone()))];
        run(&data, &models, &dir.join("out"), 1).unwrap();
        assert_eq!(calls.get(), 6);
        let again = run(&data, &models, &dir.join("out"), 1).unwrap();
        assert_eq!(calls.get(), 6, "aucun nouvel appel");
        assert_eq!(again[0].all.n, 6);
    }

    #[test]
    fn failures_are_retried_on_resume() {
        let (data, dir) = synthetic("retry", 3);
        let calls = Rc::new(Cell::new(0));
        let model = Counting::new(calls.clone());
        let fail = model.fail.clone();
        fail.set(true);
        let models: Vec<Box<dyn Classifier>> = vec![Box::new(model)];
        let first = run(&data, &models, &dir.join("out"), 1).unwrap();
        assert_eq!(first[0].all.failures, 3);
        assert_eq!(first[0].failure_kinds["transport : panne"], 3);
        fail.set(false);
        let second = run(&data, &models, &dir.join("out"), 1).unwrap();
        assert_eq!(second[0].all.failures, 0);
        assert_eq!(calls.get(), 6);
    }

    #[test]
    fn changed_signature_is_not_mixed_with_old_answers() {
        let (data, dir) = synthetic("signature", 3);
        let calls = Rc::new(Cell::new(0));
        let mut model = Counting::new(calls.clone());
        model.signature = "adresse-a".into();
        run(&data, &[Box::new(model) as Box<dyn Classifier>], &dir.join("out"), 1).unwrap();
        let mut moved = Counting::new(calls.clone());
        moved.signature = "adresse-b".into();
        run(&data, &[Box::new(moved) as Box<dyn Classifier>], &dir.join("out"), 1).unwrap();
        assert_eq!(calls.get(), 6);
    }

    #[test]
    fn mixed_model_versions_are_refused() {
        let (data, dir) = synthetic("versions", 3);
        let calls = Rc::new(Cell::new(0));
        let lines = RefCell::new(String::new());
        let mut model = Counting::new(calls.clone());
        model.version = Some("jev-1".into());
        run(&data, &[Box::new(model) as Box<dyn Classifier>], &dir.join("out"), 1).unwrap();
        // Une ligne perdue : la relance l'obtient de la nouvelle version.
        let path = dir.join("out/predictions.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        lines.borrow_mut().push_str(&text.lines().skip(1).collect::<Vec<_>>().join("\n"));
        lines.borrow_mut().push('\n');
        std::fs::write(&path, lines.borrow().as_str()).unwrap();
        let mut newer = Counting::new(calls.clone());
        newer.version = Some("jev-2".into());
        let error = run(&data, &[Box::new(newer) as Box<dyn Classifier>], &dir.join("out"), 1)
            .err()
            .unwrap();
        assert!(error.contains("versions mêlées"), "{error}");
    }

    #[test]
    fn version_is_shown_in_the_report() {
        let (data, dir) = synthetic("version_shown", 2);
        let mut model = Counting::new(Rc::new(Cell::new(0)));
        model.version = Some("jev-1.14.0".into());
        let reports = run(&data, &[Box::new(model) as Box<dyn Classifier>], &dir.join("out"), 1).unwrap();
        assert!(render(&data, &reports).contains("jev-1.14.0"));
    }

    #[test]
    fn repeated_failures_stop_the_model_without_stopping_the_run() {
        let (data, dir) = synthetic("series", 12);
        let calls = Rc::new(Cell::new(0));
        let broken = Counting::new(calls.clone());
        broken.fail.set(true);
        let models: Vec<Box<dyn Classifier>> = vec![Box::new(broken), Box::new(Mock::new(1.0, 1, &data.cases))];
        let reports = run(&data, &models, &dir.join("out"), 1).unwrap();
        assert_eq!(calls.get(), MAX_CONSECUTIVE_FAILURES as u32);
        assert_eq!(reports[0].all.failures, 12);
        assert_eq!(reports[0].failure_kinds[STOPPED], 12 - MAX_CONSECUTIVE_FAILURES);
        assert_eq!(reports[1].all.accuracy, 1.0);
    }

    struct FixedError(&'static str, Rc<Cell<u32>>);

    impl Classifier for FixedError {
        fn name(&self) -> &str {
            "refus"
        }
        fn target(&self) -> String {
            "hote".into()
        }
        fn preamble(&self) -> String {
            "consignes de test".into()
        }
        fn signature(&self) -> String {
            "sig".into()
        }
        fn predict(&self, _: &str) -> Result<Prediction, String> {
            self.1.set(self.1.get() + 1);
            Err(self.0.into())
        }
    }

    #[test]
    fn refusals_specific_to_a_case_do_not_stop_the_model() {
        let (data, dir) = synthetic("refusals", 12);
        let calls = Rc::new(Cell::new(0));
        let models: Vec<Box<dyn Classifier>> = vec![Box::new(FixedError("réponse HTTP 413", calls.clone()))];
        run(&data, &models, &dir.join("out"), 1).unwrap();
        assert_eq!(calls.get(), 12);
    }

    #[test]
    fn total_outage_gives_no_verdict() {
        let (data, dir) = synthetic("no_verdict", 30);
        let calls = Rc::new(Cell::new(0));
        let models: Vec<Box<dyn Classifier>> = vec![Box::new(FixedError("transport : panne", calls.clone()))];
        let reports = run(&data, &models, &dir.join("out"), 1).unwrap();
        let text = render(&data, &reports);
        assert!(text.contains("Pas de verdict"), "{text}");
        assert!(!text.contains("rien ne démontre"), "{text}");
    }

    #[test]
    fn missing_then_present_version_is_a_mix() {
        let (data, dir) = synthetic("none_some", 3);
        let calls = Rc::new(Cell::new(0));
        let model = Counting::new(calls.clone());
        run(&data, &[Box::new(model) as Box<dyn Classifier>], &dir.join("out"), 1).unwrap();
        let path = dir.join("out/predictions.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.lines().skip(1).collect::<Vec<_>>().join("\n") + "\n").unwrap();
        let mut newer = Counting::new(calls);
        newer.version = Some("jev-2".into());
        assert!(run(&data, &[Box::new(newer) as Box<dyn Classifier>], &dir.join("out"), 1).is_err());
    }

    #[test]
    fn output_in_the_real_state_dir_is_refused() {
        let state = crate::dataset::real_state_dir().unwrap();
        for out in [state.clone(), state.join("resultats")] {
            assert!(private_dir(&out).unwrap_err().contains("état réel"), "{out:?}");
        }
    }

    #[test]
    fn existing_output_keeps_its_permissions() {
        let (data, dir) = synthetic("perms", 1);
        let out = dir.join("partage");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::set_permissions(&out, std::fs::Permissions::from_mode(0o755)).unwrap();
        let models: Vec<Box<dyn Classifier>> = vec![Box::new(Mock::new(1.0, 1, &data.cases))];
        run(&data, &models, &out, 1).unwrap();
        assert_eq!(std::fs::metadata(&out).unwrap().permissions().mode() & 0o777, 0o755);
    }

    #[test]
    fn links_in_output_never_reach_their_target() {
        let (data, dir) = synthetic("links", 2);
        let tri = dir.join("le-vrai-tri.jsonl");
        std::fs::write(&tri, "le vrai tri\n").unwrap();
        let out = dir.join("sortie");
        std::fs::create_dir_all(&out).unwrap();
        let calls = Rc::new(Cell::new(0));
        let models = || -> Vec<Box<dyn Classifier>> { vec![Box::new(Counting::new(calls.clone()))] };

        std::os::unix::fs::symlink(&tri, out.join("predictions.jsonl")).unwrap();
        assert!(run(&data, &models(), &out, 1).err().unwrap().contains("lien"));
        std::fs::remove_file(out.join("predictions.jsonl")).unwrap();

        std::fs::hard_link(&tri, out.join("predictions.jsonl")).unwrap();
        assert!(run(&data, &models(), &out, 1).err().unwrap().contains("lien"));
        std::fs::remove_file(out.join("predictions.jsonl")).unwrap();
        assert_eq!(calls.get(), 0);

        for link in [true, false] {
            let report = out.join("report.md");
            std::fs::remove_file(&report).ok();
            if link {
                std::os::unix::fs::symlink(&tri, &report).unwrap();
            } else {
                std::fs::hard_link(&tri, &report).unwrap();
            }
            write_report(&report, "le rapport").unwrap();
            assert_eq!(std::fs::read_to_string(&report).unwrap(), "le rapport");
            assert!(!report.symlink_metadata().unwrap().file_type().is_symlink());
        }
        assert_eq!(std::fs::read_to_string(&tri).unwrap(), "le vrai tri\n");
        let mode = std::fs::metadata(out.join("report.md")).unwrap().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn truncated_predictions_file_does_not_swallow_the_next_line() {
        let (data, dir) = synthetic("truncated", 4);
        let calls = Rc::new(Cell::new(0));
        let models: Vec<Box<dyn Classifier>> = vec![Box::new(Counting::new(calls.clone()))];
        run(&data, &models, &dir.join("out"), 1).unwrap();
        assert_eq!(calls.get(), 4);
        let path = dir.join("out/predictions.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, &text[..text.len() - 20]).unwrap();
        run(&data, &models, &dir.join("out"), 1).unwrap();
        assert_eq!(calls.get(), 5);
        run(&data, &models, &dir.join("out"), 1).unwrap();
        assert_eq!(calls.get(), 5, "la reprise est stable");
    }

    #[test]
    fn empty_dataset_or_no_model_is_an_error() {
        let (mut data, dir) = synthetic("empty", 1);
        assert!(run(&data, &[], &dir.join("out"), 1).is_err());
        data.cases.clear();
        let calls = Rc::new(Cell::new(0));
        let models: Vec<Box<dyn Classifier>> = vec![Box::new(Counting::new(calls))];
        assert!(run(&data, &models, &dir.join("out"), 1).is_err());
    }

    #[test]
    fn output_inside_a_git_repo_is_refused() {
        let (data, _dir) = synthetic("in_repo", 1);
        let calls = Rc::new(Cell::new(0));
        let models: Vec<Box<dyn Classifier>> = vec![Box::new(Counting::new(calls.clone()))];
        let repo = std::env::temp_dir().join("cs_banc_faux_depot");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::os::unix::fs::symlink(&repo, std::env::temp_dir().join("cs_banc_lien_depot")).ok();
        assert!(private_dir(&std::env::temp_dir().join("cs_banc_lien_depot/par-lien")).is_err());
        let out = repo.join("sortie");
        assert!(run(&data, &models, &out, 1).err().unwrap().contains("dépôt git"));
        assert!(!out.exists());
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn verdict_is_about_cases_without_banner() {
        let (data, dir) = synthetic("verdict", 40);
        let models: Vec<Box<dyn Classifier>> = vec![Box::new(Mock::new(1.0, 1, &data.cases))];
        let reports = run(&data, &models, &dir.join("out"), 1).unwrap();
        let text = render(&data, &reports);
        let n = reports[0].without_flag.n;
        assert!(text.contains(&format!("Sur les {n} cas sans bandeau")), "{text}");
        assert!(text.contains("il bat la référence"), "{text}");
    }
}
