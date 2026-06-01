use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Active,
    Hold,
    Done,
}

/// Une ligne de `session-notes.jsonl` (métadonnées utilisateur).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionMeta {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    #[serde(default)]
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// Données extraites d'un fichier de session `.jsonl`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionInfo {
    pub path: std::path::PathBuf,
    pub session_id: String,
    pub mtime_ns: u64,
    pub size: u64,
    pub cwd: Option<String>,
    pub ai_title: Option<String>,
    pub last_prompt: Option<String>,
    pub first_user: Option<String>,
}

/// Ligne prête à l'affichage : info brute + méta utilisateur + libellé projet.
#[derive(Debug, Clone)]
pub struct SessionRow {
    pub info: SessionInfo,
    pub meta: Option<SessionMeta>,
    pub project_label: String,
}

impl SessionRow {
    pub fn status(&self) -> Status {
        self.meta.as_ref().map(|m| m.status).unwrap_or_default()
    }

    /// Résumé affiché : note locale > titre auto > dernier prompt/1er msg > "(vide)".
    pub fn summary(&self) -> String {
        let raw = self
            .meta
            .as_ref()
            .and_then(|m| m.note.clone())
            .or_else(|| self.info.ai_title.clone())
            .or_else(|| self.info.last_prompt.clone())
            .or_else(|| self.info.first_user.clone())
            .unwrap_or_else(|| "(vide)".to_string());
        let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        collapsed.chars().take(70).collect()
    }

    /// Clé de tri : hold (0) avant active (1) avant done (2), puis mtime décroissant.
    pub fn sort_key(&self) -> (u8, std::cmp::Reverse<u64>) {
        let rank = match self.status() {
            Status::Hold => 0,
            Status::Active => 1,
            Status::Done => 2,
        };
        (rank, std::cmp::Reverse(self.info.mtime_ns))
    }
}

#[cfg(test)]
mod row_tests {
    use super::*;
    use std::path::PathBuf;

    fn info(mtime: u64) -> SessionInfo {
        SessionInfo {
            path: PathBuf::from("x"),
            session_id: "s".into(),
            mtime_ns: mtime,
            size: 3000,
            cwd: Some("/p".into()),
            ai_title: Some("Titre".into()),
            last_prompt: None,
            first_user: None,
        }
    }

    #[test]
    fn note_beats_title_in_summary() {
        let row = SessionRow {
            info: info(1),
            meta: Some(SessionMeta {
                session_id: "s".into(),
                updated_at: "t".into(),
                status: Status::Active,
                note: Some("ma note".into()),
            }),
            project_label: "proj".into(),
        };
        assert_eq!(row.summary(), "ma note");
    }

    #[test]
    fn hold_sorts_before_active() {
        let mut hold = SessionRow { info: info(1), meta: Some(SessionMeta { session_id: "s".into(), updated_at: "t".into(), status: Status::Hold, note: None }), project_label: "p".into() };
        let active = SessionRow { info: info(999), meta: None, project_label: "p".into() };
        assert!(hold.sort_key() < active.sort_key());
        hold.info.mtime_ns = 0;
        assert!(hold.sort_key() < active.sort_key()); // statut prime sur mtime
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Status::Hold).unwrap(), "\"hold\"");
    }

    #[test]
    fn meta_roundtrip_with_defaults() {
        let line = r#"{"sessionId":"abc","updatedAt":"2026-06-01T13:45","status":"hold","note":"reprendre A1.3"}"#;
        let m: SessionMeta = serde_json::from_str(line).unwrap();
        assert_eq!(m.status, Status::Hold);
        assert_eq!(m.note.as_deref(), Some("reprendre A1.3"));
    }

    #[test]
    fn meta_missing_status_defaults_active() {
        let line = r#"{"sessionId":"abc","updatedAt":"2026-06-01T13:45"}"#;
        let m: SessionMeta = serde_json::from_str(line).unwrap();
        assert_eq!(m.status, Status::Active);
        assert_eq!(m.note, None);
    }

    #[test]
    fn meta_note_absent_when_none() {
        let m = SessionMeta {
            session_id: "abc".into(),
            updated_at: "2026-06-01T13:45".into(),
            status: Status::Active,
            note: None,
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(!s.contains("note"), "note key must be absent when None: {s}");
    }
}
