use crate::transcript::Digest;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    #[default]
    Active,
    Hold,
    Done,
    /// Prête à être lancée/exécutée (🔥).
    #[serde(rename = "ready_to_burn")]
    ReadyToBurn,
    /// Nécessite une intervention manuelle (🔍).
    #[serde(rename = "needs_manual_work")]
    NeedsManualWork,
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
    /// Date de la dernière décision de l'utilisateur (statut posé). Distincte
    /// d'`updatedAt`, qu'une simple modification de note fait bouger.
    #[serde(rename = "decidedAt", skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<String>,
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
    #[serde(default)]
    pub digest: Digest,
    /// Hash des derniers octets lus : détecte un fichier réécrit à taille égale.
    #[serde(default)]
    pub tail_hash: u64,
}

impl SessionInfo {
    /// Titre à afficher : celui donné à la main, sinon le plus récent généré.
    pub fn title(&self) -> Option<&str> {
        self.digest
            .custom_title
            .as_deref()
            .or(self.digest.ai_title.as_deref())
            .or(self.ai_title.as_deref())
    }

    pub fn last_real_ms(&self) -> Option<i64> {
        self.digest.last_real_at_ms
    }

    /// Dernier signe d'activité de l'utilisateur. La date du fichier bouge toute
    /// seule : elle ne sert qu'en repli, pour une session sans vrai message.
    pub fn activity_ns(&self) -> u64 {
        self.last_real_ms()
            .map(|ms| (ms.max(0) as u64).saturating_mul(1_000_000))
            .unwrap_or(self.mtime_ns)
    }
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

    /// Clé de tri : ce qui réclame mon attention d'abord (needs-manual 🔍, ready-to-burn 🔥,
    /// hold ⏳), puis active, puis done en bas ; à statut égal, mtime décroissant.
    pub fn sort_key(&self) -> (u8, std::cmp::Reverse<u64>) {
        let rank = match self.status() {
            Status::NeedsManualWork => 0,
            Status::ReadyToBurn => 1,
            Status::Hold => 2,
            Status::Active => 3,
            Status::Done => 4,
        };
        (rank, std::cmp::Reverse(self.info.activity_ns()))
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
            digest: Default::default(),
            tail_hash: 0,
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
                decided_at: None,
            }),
            project_label: "proj".into(),
        };
        assert_eq!(row.summary(), "ma note");
    }

    #[test]
    fn activity_prefers_last_real_message_over_file_date() {
        let mut i = info(9_000_000_000_000_000);
        assert_eq!(i.activity_ns(), 9_000_000_000_000_000);
        i.digest.last_real_at_ms = Some(1_000);
        assert_eq!(i.activity_ns(), 1_000_000_000);
    }

    #[test]
    fn hold_sorts_before_active() {
        let mut hold = SessionRow {
            info: info(1),
            meta: Some(SessionMeta {
                session_id: "s".into(),
                updated_at: "t".into(),
                status: Status::Hold,
                note: None,
                decided_at: None,
            }),
            project_label: "p".into(),
        };
        let active = SessionRow {
            info: info(999),
            meta: None,
            project_label: "p".into(),
        };
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
    fn new_statuses_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&Status::ReadyToBurn).unwrap(),
            "\"ready_to_burn\""
        );
        assert_eq!(
            serde_json::to_string(&Status::NeedsManualWork).unwrap(),
            "\"needs_manual_work\""
        );
        // round-trip : le format on-disk doit se relire à l'identique
        let s: Status = serde_json::from_str("\"ready_to_burn\"").unwrap();
        assert_eq!(s, Status::ReadyToBurn);
        let s: Status = serde_json::from_str("\"needs_manual_work\"").unwrap();
        assert_eq!(s, Status::NeedsManualWork);
    }

    #[test]
    fn catalog_has_each_status_exactly_once() {
        let keys: Vec<String> = [
            Status::Active,
            Status::Hold,
            Status::ReadyToBurn,
            Status::NeedsManualWork,
            Status::Done,
        ]
        .iter()
        .map(|s| serde_json::to_string(s).unwrap().trim_matches('"').to_string())
        .collect();
        let catalog: Vec<String> = crate::categories::CATEGORIES.iter().map(|(k, _)| k.to_string()).collect();
        assert_eq!(keys, catalog);
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
            decided_at: None,
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(
            !s.contains("note"),
            "note key must be absent when None: {s}"
        );
    }
}
