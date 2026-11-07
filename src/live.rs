//! Sessions ouvertes, d'après le registre de Claude Code (`~/.claude/sessions`).
//! Ce fichier est interne et non documenté : s'il change, l'indicateur s'éteint
//! sans rien casser d'autre.
use crate::paths::Paths;
use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};
use std::collections::{HashMap, HashSet};

/// Une entrée du registre.
#[derive(Debug, PartialEq)]
pub struct Entry {
    pub pid: i32,
    pub session_id: String,
    /// Date de démarrage du processus, en UTC.
    pub started: Option<NaiveDateTime>,
}

const START_FORMAT: &str = "%a %b %e %H:%M:%S %Y";
const START_TOLERANCE_SECS: i64 = 2;
/// `ps` échoue en bloc sur macOS si un PID dépasse sa limite.
const MACOS_PID_LIMIT: i32 = 100_000;

/// Lit le premier objet JSON : des fichiers réels portent une accolade de trop après
/// un objet complet. Écarte les processus d'arrière-plan (`kind` = `bg`).
pub fn parse_entry(json: &str) -> Option<Entry> {
    let v: serde_json::Value = serde_json::Deserializer::from_str(json)
        .into_iter()
        .next()?
        .ok()?;
    if v.get("kind")?.as_str()? != "interactive" {
        return None;
    }
    let pid = i32::try_from(v.get("pid")?.as_i64()?).ok()?;
    if pid <= 0 || (cfg!(target_os = "macos") && pid >= MACOS_PID_LIMIT) {
        return None;
    }
    let started = v
        .get("procStart")
        .and_then(|s| s.as_str())
        .and_then(|s| NaiveDateTime::parse_from_str(s, START_FORMAT).ok());
    Some(Entry {
        pid,
        session_id: v.get("sessionId")?.as_str()?.to_string(),
        started,
    })
}

/// Une ligne de `ps -o pid=,lstart=` : le PID et sa date de démarrage, en UTC.
fn parse_ps_line(line: &str) -> Option<(i32, DateTime<Utc>)> {
    let line = line.trim_start();
    let (pid, start) = line.split_once(char::is_whitespace)?;
    let naive = NaiveDateTime::parse_from_str(start.trim(), START_FORMAT).ok()?;
    let local = Local.from_local_datetime(&naive).earliest()?;
    Some((pid.parse().ok()?, local.with_timezone(&Utc)))
}

/// Un PID réutilisé après un redémarrage appartient à un processus plus récent :
/// sans date dans le registre, on se fie au PID seul.
fn same_process(entry: &Entry, running: DateTime<Utc>) -> bool {
    entry.started.is_none_or(|started| {
        (running.naive_utc() - started).num_seconds().abs() <= START_TOLERANCE_SECS
    })
}

fn running_processes(entries: &[Entry]) -> HashMap<i32, DateTime<Utc>> {
    if entries.is_empty() {
        return HashMap::new();
    }
    let list = entries.iter().map(|e| e.pid.to_string()).collect::<Vec<_>>().join(",");
    // Un seul appel à `ps` : le picker le relance à chaque tour de boucle.
    let Ok(out) = std::process::Command::new("ps")
        .env("LC_ALL", "C")
        .args(["-o", "pid=,lstart=", "-p", &list])
        .output()
    else {
        return HashMap::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(parse_ps_line)
        .collect()
}

pub fn open_sessions(paths: &Paths) -> HashSet<String> {
    let Ok(files) = std::fs::read_dir(&paths.registry) else {
        return HashSet::new();
    };
    let entries: Vec<Entry> = files
        .flatten()
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|json| parse_entry(&json))
        .collect();
    let running = running_processes(&entries);
    entries
        .into_iter()
        .filter(|e| running.get(&e.pid).is_some_and(|at| same_process(e, *at)))
        .map(|e| e.session_id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTRY: &str =
        r#"{"pid":207,"sessionId":"abc","kind":"interactive","procStart":"Thu Oct 22 00:06:12 2026"}"#;

    #[test]
    fn interactive_entry_is_kept() {
        let e = parse_entry(ENTRY).unwrap();
        assert_eq!((e.pid, e.session_id.as_str()), (207, "abc"));
        assert!(e.started.is_some());
    }

    #[test]
    fn trailing_garbage_after_object_is_tolerated() {
        assert_eq!(parse_entry(&format!("{ENTRY}}}")).unwrap().pid, 207);
    }

    #[test]
    fn bg_spare_is_dropped() {
        assert_eq!(parse_entry(r#"{"pid":207,"sessionId":"abc","kind":"bg"}"#), None);
    }

    #[test]
    fn malformed_file_is_ignored() {
        assert_eq!(parse_entry("pas du json"), None);
        assert_eq!(parse_entry(r#"{"pid":"x"}"#), None);
    }

    #[test]
    fn reused_pid_is_not_the_same_process() {
        let e = parse_entry(ENTRY).unwrap();
        let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc);
        assert!(same_process(&e, at("2026-10-22T00:06:13Z")));
        assert!(!same_process(&e, at("2026-11-07T10:00:00Z")));
    }

    #[test]
    fn own_process_is_detected_through_ps() {
        // Le bac à sable de build de Nix n'a pas `ps`.
        if std::process::Command::new("ps").arg("-p").arg("1").output().is_err() {
            return;
        }
        let pid = std::process::id() as i32;
        let e = Entry { pid, session_id: "s".into(), started: None };
        assert!(running_processes(&[e]).contains_key(&pid));
    }

    #[test]
    fn ps_line_is_parsed() {
        let (pid, _) = parse_ps_line("  207 Thu Oct 22 02:06:12 2026").unwrap();
        assert_eq!(pid, 207);
    }

    #[test]
    fn missing_registry_gives_empty_set() {
        let p = Paths::sandbox(std::path::Path::new("/n/existe/pas"));
        assert!(open_sessions(&p).is_empty());
    }
}
