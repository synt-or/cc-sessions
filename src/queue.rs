//! Quelles sessions l'utilisateur doit-il revoir, et dans quel ordre ?
use crate::model::{SessionInfo, SessionMeta, SessionRow, Status};
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use std::path::Path;

const VOICE_BRIDGE_PREFIX: &str = "claude-voice-msg";
const LEGACY_STAMP: &str = "%Y-%m-%dT%H:%M";
const ONE_MINUTE_MS: i64 = 60_000;

/// Seule règle du périmètre : tout sauf le pont vocal, dont les sessions ne
/// font que transférer une donnée. Une vraie session lancée à la voix travaille
/// dans un projet normal et reste donc dedans, quel que soit son nombre de
/// messages. `--all` montre le pont.
pub fn in_scope(info: &SessionInfo) -> bool {
    let from_cwd = info
        .cwd
        .as_deref()
        .and_then(|c| Path::new(c).file_name())
        .is_some_and(|n| n.to_string_lossy().starts_with(VOICE_BRIDGE_PREFIX));
    let from_project = info
        .path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|n| n.to_string_lossy().contains(&format!("-{VOICE_BRIDGE_PREFIX}")));
    !(from_cwd || from_project)
}

/// Instant après lequel un message de l'utilisateur vaut reprise. `None` : aucune
/// décision enregistrée.
pub fn decision_after_ms(meta: &SessionMeta) -> Option<i64> {
    if let Some(at) = &meta.decided_at {
        return DateTime::parse_from_rfc3339(at).ok().map(|d| d.timestamp_millis());
    }
    if meta.status == Status::Active {
        return None;
    }
    // Ancienne entrée : `updatedAt` est à la minute, en heure locale. Un message
    // de la même minute est considéré comme antérieur au flag.
    let naive = NaiveDateTime::parse_from_str(&meta.updated_at, LEGACY_STAMP).ok()?;
    let local = Local.from_local_datetime(&naive).earliest()?;
    Some(local.timestamp_millis() + ONE_MINUTE_MS)
}

/// À revoir si aucune décision n'existe, ou si l'utilisateur a écrit depuis la dernière.
pub fn needs_review(row: &SessionRow) -> bool {
    match row.meta.as_ref().and_then(decision_after_ms) {
        None => true,
        Some(after) => row.info.last_real_ms().is_some_and(|ms| ms > after),
    }
}

/// La file : dernier vrai message le plus récent d'abord ; les sessions sans
/// vrai message n'ont pas de date comparable et passent à la fin.
pub fn build(rows: Vec<SessionRow>) -> Vec<SessionRow> {
    let mut queue: Vec<SessionRow> = rows.into_iter().filter(needs_review).collect();
    queue.sort_by_key(|r| {
        (
            r.info.last_real_ms().is_none(),
            std::cmp::Reverse(r.info.last_real_ms().unwrap_or(0)),
            std::cmp::Reverse(r.info.mtime_ns),
        )
    });
    queue
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::path::PathBuf;

    pub fn info(sid: &str, cwd: &str, last_real_ms: Option<i64>) -> SessionInfo {
        SessionInfo {
            path: PathBuf::from(format!("/projects/-Users-x-repo/{sid}.jsonl")),
            session_id: sid.into(),
            mtime_ns: 5,
            size: 3000,
            cwd: Some(cwd.into()),
            ai_title: None,
            last_prompt: None,
            first_user: None,
            digest: crate::transcript::Digest {
                real_count: u32::from(last_real_ms.is_some()),
                last_real_at_ms: last_real_ms,
                offset: 0,
                ..Default::default()
            },
            tail_hash: 0,
        }
    }

    pub fn row(sid: &str, last_real_ms: Option<i64>, meta: Option<SessionMeta>) -> SessionRow {
        SessionRow {
            info: info(sid, "/Users/x/repo", last_real_ms),
            meta,
            project_label: "repo".into(),
        }
    }

    fn decided(at: &str, status: Status) -> SessionMeta {
        SessionMeta {
            session_id: "s".into(),
            updated_at: "2026-06-01T10:00".into(),
            status,
            note: None,
            decided_at: Some(at.into()),
        }
    }

    fn ms(at: &str) -> i64 {
        DateTime::parse_from_rfc3339(at).unwrap().timestamp_millis()
    }

    #[test]
    fn undecided_session_needs_review() {
        assert!(needs_review(&row("s", Some(1), None)));
        assert!(needs_review(&row("s", None, None)));
    }

    #[test]
    fn decided_without_new_message_is_out() {
        let at = "2026-10-12T14:32:00+02:00";
        let meta = decided(at, Status::Hold);
        assert!(!needs_review(&row("s", Some(ms(at) - 1), Some(meta.clone()))));
        assert!(!needs_review(&row("s", None, Some(meta))));
    }

    #[test]
    fn decided_then_resumed_is_in() {
        let at = "2026-10-12T14:32:00+02:00";
        for status in [Status::Hold, Status::ReadyToBurn, Status::NeedsManualWork, Status::Done, Status::Active] {
            assert!(
                needs_review(&row("s", Some(ms(at) + 1), Some(decided(at, status)))),
                "{status:?}"
            );
        }
    }

    #[test]
    fn active_with_decision_leaves_queue() {
        let at = "2026-10-12T14:32:00+02:00";
        assert!(!needs_review(&row("s", Some(ms(at) - 1), Some(decided(at, Status::Active)))));
    }

    #[test]
    fn active_without_decided_at_needs_review() {
        let meta = SessionMeta {
            session_id: "s".into(),
            updated_at: "2026-06-01T10:00".into(),
            status: Status::Active,
            note: Some("note seule".into()),
            decided_at: None,
        };
        assert!(needs_review(&row("s", None, Some(meta))));
    }

    #[test]
    fn legacy_same_minute_message_counts_as_before() {
        let meta = SessionMeta {
            session_id: "s".into(),
            updated_at: "2026-06-01T10:00".into(),
            status: Status::Hold,
            note: None,
            decided_at: None,
        };
        let naive = NaiveDateTime::parse_from_str("2026-06-01T10:00", LEGACY_STAMP).unwrap();
        let flag = Local.from_local_datetime(&naive).earliest().unwrap().timestamp_millis();
        assert!(!needs_review(&row("s", Some(flag + 30_000), Some(meta.clone()))));
        assert!(needs_review(&row("s", Some(flag + 60_001), Some(meta))));
    }

    #[test]
    fn sessions_without_real_message_sort_last() {
        let q = build(vec![row("vide", None, None), row("vieux", Some(1), None), row("neuf", Some(9), None)]);
        let order: Vec<_> = q.iter().map(|r| r.info.session_id.as_str()).collect();
        assert_eq!(order, ["neuf", "vieux", "vide"]);
    }

    #[test]
    fn voice_bridge_is_out_of_scope() {
        let mut i = info("s", "/private/var/folders/x/T/claude-voice-msg-side-ab12", Some(1));
        i.digest.real_count = 20;
        assert!(!in_scope(&i));
        assert!(!in_scope(&info("s", "/private/var/folders/x/T/claude-voice-msg-ykjy0", Some(1))));
        // cwd absent : le nom du dossier projet suffit
        let mut by_project = info("s", "/x", None);
        by_project.cwd = None;
        by_project.path = PathBuf::from("/p/-private-var-folders-x-T-claude-voice-msg-digest-a/s.jsonl");
        assert!(!in_scope(&by_project));
    }

    #[test]
    fn real_voice_session_stays_in_scope() {
        let mut i = info("s", "/Users/x/projets/audit", Some(1));
        i.digest.real_count = 2;
        assert!(in_scope(&i));
        assert!(in_scope(&info("s", "/Users/x/projets/claude-code-voice-mode", None)));
    }
}
