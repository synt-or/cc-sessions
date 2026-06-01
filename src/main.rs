mod archive;
mod build;
mod cache;
mod cli;
mod meta;
mod model;
mod picker;
mod project;
mod scan;
mod stats;

use anyhow::Result;
use clap::Parser;
use model::Status;
use std::path::PathBuf;

fn projects_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".claude").join("projects")
}

/// Horodatage léger sans dépendance chrono : `date '+%Y-%m-%dT%H:%M'`.
fn now_stamp() -> String {
    std::process::Command::new("date")
        .args(["+%Y-%m-%dT%H:%M"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Exposé pour le module stats.
pub fn now_stamp_pub() -> String {
    now_stamp()
}

/// Trouve le cwd d'une session via l'index (pour rattacher note/statut au bon projet).
fn cwd_of_session(sid: &str) -> Option<String> {
    let idx = cache::load(&cache::index_path());
    idx.values().find(|i| i.session_id == sid).and_then(|i| i.cwd.clone())
}

fn set_status(status: Status) -> Result<()> {
    let sid = cli::current_session_id()?;
    let cwd = cwd_of_session(&sid).unwrap_or_else(|| ".".to_string());
    let existing = meta::load(&meta::notes_file(&cwd)).get(&sid).and_then(|m| m.note.clone());
    meta::upsert(&cwd, &sid, &now_stamp(), status, existing)?;
    println!("✓ statut={status:?} pour {sid}");
    Ok(())
}

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    match cli.command {
        None => picker::run(&build::rows(&projects_dir())),
        Some(cli::Command::Note { text, append }) => {
            let sid = cli::current_session_id()?;
            let cwd = cwd_of_session(&sid).unwrap_or_else(|| ".".to_string());
            let mut note = text.join(" ");
            if append {
                if let Some(prev) = meta::load(&meta::notes_file(&cwd)).get(&sid).and_then(|m| m.note.clone()) {
                    note = format!("{prev} ⏎ {note}");
                }
            }
            let status = meta::load(&meta::notes_file(&cwd)).get(&sid).map(|m| m.status).unwrap_or_default();
            let m = meta::upsert(&cwd, &sid, &now_stamp(), status, Some(note))?;
            println!("✓ note ({sid})\n  {}", m.note.unwrap_or_default());
            Ok(())
        }
        Some(cli::Command::Hold) => set_status(Status::Hold),
        Some(cli::Command::Active) => set_status(Status::Active),
        Some(cli::Command::Done { older_than }) => match older_than {
            Some(spec) => stats::mark_done_older_than(&projects_dir(), &spec),
            None => set_status(Status::Done),
        },
        Some(cli::Command::Stats) => stats::print_stats(&projects_dir()),
        Some(cli::Command::Archive { older_than, uuids }) => archive::archive(&projects_dir(), older_than.as_deref(), &uuids),
        Some(cli::Command::PurgeArchive) => archive::purge_archive(),
    }
}
