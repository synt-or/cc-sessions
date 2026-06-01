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
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
}
