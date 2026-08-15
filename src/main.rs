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
    dirs::home_dir()
        .unwrap_or_default()
        .join(".claude")
        .join("projects")
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
    idx.values()
        .find(|i| i.session_id == sid)
        .and_then(|i| i.cwd.clone())
}

/// Résout (session_id, cwd) de la session ciblée.
///
/// `--id <uuid>` : la session DOIT être retrouvable dans l'index (rafraîchi au
/// besoin) — un fallback silencieux vers `.` rattacherait la note/le statut au
/// mauvais projet. Sans `--id` : session courante via $CLAUDE_CODE_SESSION_ID,
/// avec fallback `.` historique (une session toute neuve, < 2 Kio, n'est pas
/// encore indexée mais son cwd est là où on tape la commande).
fn target_session(explicit: Option<String>) -> Result<(String, String)> {
    match explicit {
        Some(sid) => {
            let cwd = cwd_of_session(&sid).or_else(|| {
                let _ = build::rows(&projects_dir(), None); // rafraîchit l'index
                cwd_of_session(&sid)
            });
            match cwd {
                Some(c) => Ok((sid, c)),
                None => anyhow::bail!("session {sid} introuvable dans ~/.claude/projects"),
            }
        }
        None => {
            let sid = cli::current_session_id()?;
            let cwd = cwd_of_session(&sid).unwrap_or_else(|| ".".to_string());
            Ok((sid, cwd))
        }
    }
}

fn set_status(id: Option<String>, status: Status) -> Result<()> {
    let (sid, cwd) = target_session(id)?;
    let existing = meta::load(&meta::notes_file(&cwd))
        .get(&sid)
        .and_then(|m| m.note.clone());
    meta::upsert(&cwd, &sid, &now_stamp(), status, existing)?;
    println!("✓ statut={status:?} pour {sid}");
    Ok(())
}

/// Périmètre du filtre `--local` : le repo courant, ou le pwd hors repo.
fn local_scope() -> Result<String> {
    let cwd = std::env::current_dir()?;
    let cwd = cwd.to_string_lossy();
    project::scope_root(&cwd)
        .ok_or_else(|| anyhow::anyhow!("--local : répertoire courant introuvable ({cwd})"))
}

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    match cli.command {
        None => {
            let scope = if cli.local { Some(local_scope()?) } else { None };
            picker::run(&projects_dir(), cli.reverse, scope.as_deref())
        }
        Some(cli::Command::Note { text, append, done }) => {
            let (sid, cwd) = target_session(cli.id)?;
            let mut note = text.join(" ");
            if append {
                if let Some(prev) = meta::load(&meta::notes_file(&cwd))
                    .get(&sid)
                    .and_then(|m| m.note.clone())
                {
                    note = format!("{prev} ⏎ {note}");
                }
            }
            let status = if done {
                Status::Done
            } else {
                meta::load(&meta::notes_file(&cwd))
                    .get(&sid)
                    .map(|m| m.status)
                    .unwrap_or_default()
            };
            let m = meta::upsert(&cwd, &sid, &now_stamp(), status, Some(note))?;
            println!("✓ note ({sid})\n  {}", m.note.unwrap_or_default());
            if done {
                println!("✓ statut=Done pour {sid}");
            }
            Ok(())
        }
        Some(cli::Command::Hold) => set_status(cli.id, Status::Hold),
        Some(cli::Command::Active) => set_status(cli.id, Status::Active),
        Some(cli::Command::Burn) => set_status(cli.id, Status::ReadyToBurn),
        Some(cli::Command::Manual) => set_status(cli.id, Status::NeedsManualWork),
        Some(cli::Command::Done { older_than }) => match older_than {
            Some(spec) => stats::mark_done_older_than(&projects_dir(), &spec),
            None => set_status(cli.id, Status::Done),
        },
        Some(cli::Command::Stats) => stats::print_stats(&projects_dir()),
        Some(cli::Command::Archive { older_than, uuids }) => {
            archive::archive(&projects_dir(), older_than.as_deref(), &uuids)
        }
        Some(cli::Command::PurgeArchive) => archive::purge_archive(),
    }
}
