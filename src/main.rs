mod archive;
mod build;
mod cache;
#[cfg(test)]
mod categories;
mod cli;
mod journal;
mod live;
mod meta;
mod model;
mod paths;
mod picker;
mod project;
mod queue;
mod review;
mod scan;
mod snapshot;
mod stats;
mod transcript;

use anyhow::Result;
use clap::Parser;
use model::Status;
use paths::Paths;

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
fn cwd_of_session(paths: &Paths, sid: &str) -> Option<String> {
    let idx = cache::load(&paths.cache_index);
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
fn target_session(paths: &Paths, explicit: Option<String>) -> Result<(String, String)> {
    match explicit {
        Some(sid) => {
            let cwd = cwd_of_session(paths, &sid).or_else(|| {
                let _ = build::all_rows(paths, None); // rafraîchit l'index
                cwd_of_session(paths, &sid)
            });
            match cwd {
                Some(c) => Ok((sid, c)),
                None => anyhow::bail!("session {sid} introuvable dans ~/.claude/projects"),
            }
        }
        None => {
            let sid = cli::current_session_id()?;
            let cwd = cwd_of_session(paths, &sid)
                .or_else(|| {
                    // Index vide ou en retard : sans ce rafraîchissement, le repli
                    // sur `.` écrirait la note dans le dépôt courant.
                    let _ = build::all_rows(paths, None);
                    cwd_of_session(paths, &sid)
                })
                .unwrap_or_else(|| ".".to_string());
            Ok((sid, cwd))
        }
    }
}

fn set_status(paths: &Paths, id: Option<String>, status: Status) -> Result<()> {
    let (sid, cwd) = target_session(paths, id)?;
    meta::set_status(paths, &cwd, &sid, status)?;
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
    let paths = Paths::from_env()?;
    let paths = &paths;
    if cli.undo {
        if !matches!(cli.command, None | Some(cli::Command::Review)) {
            anyhow::bail!("-z annule une décision de revue : à utiliser seul");
        }
        let scope = if cli.local { Some(local_scope()?) } else { None };
        return review::run(paths, scope.as_deref(), cli.all, true);
    }
    match cli.command {
        None => {
            let scope = if cli.local { Some(local_scope()?) } else { None };
            picker::run(paths, cli.reverse, scope.as_deref(), cli.all)
        }
        Some(cli::Command::Review) => {
            let scope = if cli.local { Some(local_scope()?) } else { None };
            review::run(paths, scope.as_deref(), cli.all, false)
        }
        Some(cli::Command::Note { text, append, done }) => {
            let (sid, cwd) = target_session(paths, cli.id)?;
            let mut note = text.join(" ");
            if append {
                if let Some(prev) = meta::load_for(paths, &cwd, &sid).and_then(|m| m.note) {
                    note = format!("{prev} ⏎ {note}");
                }
            }
            let mut m = meta::set_note(paths, &cwd, &sid, Some(note))?;
            if done {
                m = meta::set_status(paths, &cwd, &sid, Status::Done)?;
            }
            println!("✓ note ({sid})\n  {}", m.note.unwrap_or_default());
            if done {
                println!("✓ statut=Done pour {sid}");
            }
            Ok(())
        }
        Some(cli::Command::Hold) => set_status(paths, cli.id, Status::Hold),
        Some(cli::Command::Active) => set_status(paths, cli.id, Status::Active),
        Some(cli::Command::Burn) => set_status(paths, cli.id, Status::ReadyToBurn),
        Some(cli::Command::Manual) => set_status(paths, cli.id, Status::NeedsManualWork),
        Some(cli::Command::Done { older_than }) => match older_than {
            Some(spec) => stats::mark_done_older_than(paths, &spec),
            None => set_status(paths, cli.id, Status::Done),
        },
        Some(cli::Command::Stats) => stats::print_stats(paths),
        Some(cli::Command::Archive { older_than, uuids }) => {
            archive::archive(paths, older_than.as_deref(), &uuids)
        }
        Some(cli::Command::PurgeArchive) => archive::purge_archive(paths),
    }
}
