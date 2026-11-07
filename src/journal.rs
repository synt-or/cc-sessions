//! Deux fichiers sous `state_dir`, deux rôles :
//! - `review-journal.jsonl` : pile d'annulation de la revue, réécrite à chaque `z`.
//! - `review-decisions.jsonl` : historique en ajout seul des catégories choisies,
//!   pour évaluer plus tard des classifieurs. Jamais de contenu de transcript :
//!   seulement le hash de l'instantané présenté (cf. `snapshot`).
use crate::meta;
use crate::model::{SessionMeta, Status};
use crate::paths::Paths;
use crate::snapshot;
use serde::{Deserialize, Serialize};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub status: Status,
    pub note: Option<String>,
    pub decided_at: Option<String>,
}

impl Snapshot {
    fn of(meta: Option<SessionMeta>) -> Snapshot {
        meta.map(|m| Snapshot {
            decided_at: meta::effective_decided_at(&m),
            status: m.status,
            note: m.note,
        })
        .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Status,
    Note,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub session_id: String,
    pub cwd: String,
    pub kind: Kind,
    pub before: Snapshot,
    pub after: Snapshot,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "line", rename_all = "snake_case")]
pub enum Line {
    Decision(Entry),
    RoundEnd,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UndoPlan {
    Empty,
    Clean(Entry),
    /// Le champ a changé depuis la décision : l'utilisateur doit voir les deux valeurs.
    Conflict { entry: Entry, current: Snapshot },
}

/// Ce que l'utilisateur avait à l'écran au moment de décider.
#[derive(Debug, Clone, PartialEq)]
pub struct Presented {
    pub snapshot_hash: String,
    pub snapshot_stored: bool,
    pub transcript_bytes: u64,
}

#[derive(Serialize)]
struct DecisionRecord<'a> {
    #[serde(rename = "sessionId")]
    session_id: &'a str,
    event: &'a str,
    category: Status,
    at: String,
    #[serde(rename = "snapshotVersion", skip_serializing_if = "Option::is_none")]
    snapshot_version: Option<u32>,
    #[serde(rename = "snapshotHash", skip_serializing_if = "Option::is_none")]
    snapshot_hash: Option<&'a str>,
    #[serde(rename = "snapshotStored", skip_serializing_if = "Option::is_none")]
    snapshot_stored: Option<bool>,
    #[serde(rename = "transcriptBytes", skip_serializing_if = "Option::is_none")]
    transcript_bytes: Option<u64>,
}

pub struct Journal {
    path: PathBuf,
    decisions: PathBuf,
    lines: Vec<Line>,
    /// Dernier échec non bloquant, à afficher par l'appelant.
    pub warning: Option<String>,
}

impl Journal {
    pub fn open(paths: &Paths) -> Journal {
        let path = paths.state_dir.join("review-journal.jsonl");
        let lines = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        Journal {
            path,
            decisions: paths.state_dir.join("review-decisions.jsonl"),
            lines,
            warning: None,
        }
    }

    fn append(path: &PathBuf, json: &str) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        writeln!(f, "{json}")
    }

    pub fn record(&mut self, line: Line) -> io::Result<()> {
        Self::append(&self.path, &serde_json::to_string(&line).unwrap())?;
        self.lines.push(line);
        Ok(())
    }

    /// L'historique sert à évaluer les classifieurs : son échec ne doit ni bloquer
    /// le tri ni laisser croire que la décision a échoué. Elle est bien appliquée.
    fn log_decision(&mut self, record: &DecisionRecord) {
        if let Err(e) = Self::append(&self.decisions, &serde_json::to_string(record).unwrap()) {
            self.warning = Some(format!("historique non écrit : {e}"));
        }
    }

    /// Pose un statut depuis la revue : l'applique, l'empile, l'historise.
    pub fn decide_status(
        &mut self,
        paths: &Paths,
        cwd: &str,
        sid: &str,
        status: Status,
        presented: &Presented,
    ) -> io::Result<SessionMeta> {
        let before = Snapshot::of(meta::load_for(paths, cwd, sid));
        let written = meta::set_status(paths, cwd, sid, status)?;
        self.record(Line::Decision(Entry {
            session_id: sid.to_string(),
            cwd: cwd.to_string(),
            kind: Kind::Status,
            before,
            after: Snapshot::of(Some(written.clone())),
        }))?;
        self.log_decision(&DecisionRecord {
            session_id: sid,
            event: "decision",
            category: status,
            at: written.decided_at.clone().unwrap_or_default(),
            snapshot_version: Some(snapshot::VERSION),
            snapshot_hash: Some(&presented.snapshot_hash),
            snapshot_stored: Some(presented.snapshot_stored),
            transcript_bytes: Some(presented.transcript_bytes),
        });
        Ok(written)
    }

    /// Écrit une note depuis la revue et l'empile (annulable, sans compter comme décision).
    pub fn decide_note(
        &mut self,
        paths: &Paths,
        cwd: &str,
        sid: &str,
        note: Option<String>,
    ) -> io::Result<SessionMeta> {
        let before = Snapshot::of(meta::load_for(paths, cwd, sid));
        let written = meta::set_note(paths, cwd, sid, note)?;
        self.record(Line::Decision(Entry {
            session_id: sid.to_string(),
            cwd: cwd.to_string(),
            kind: Kind::Note,
            before,
            after: Snapshot::of(Some(written.clone())),
        }))?;
        Ok(written)
    }

    /// Décisions du tour en cours (les notes ne comptent pas).
    pub fn round_done(&self) -> u32 {
        self.lines
            .iter()
            .rev()
            .take_while(|l| !matches!(l, Line::RoundEnd))
            .filter(|l| matches!(l, Line::Decision(e) if e.kind == Kind::Status))
            .count() as u32
    }

    fn last_entry(&self) -> Option<&Entry> {
        self.lines.iter().rev().find_map(|l| match l {
            Line::Decision(e) => Some(e),
            Line::RoundEnd => None,
        })
    }

    pub fn plan_undo(&self, paths: &Paths) -> UndoPlan {
        let Some(entry) = self.last_entry() else {
            return UndoPlan::Empty;
        };
        let current = Snapshot::of(meta::load_for(paths, &entry.cwd, &entry.session_id));
        let untouched = match entry.kind {
            Kind::Status => {
                current.status == entry.after.status && current.decided_at == entry.after.decided_at
            }
            Kind::Note => current.note == entry.after.note,
        };
        if untouched {
            UndoPlan::Clean(entry.clone())
        } else {
            UndoPlan::Conflict {
                entry: entry.clone(),
                current,
            }
        }
    }

    /// Annule la dernière décision : ne restaure que les champs qu'elle a
    /// changés, retire les fins de tour qui la suivent. Renvoie l'id de session.
    pub fn apply_undo(&mut self, paths: &Paths) -> io::Result<Option<String>> {
        let Some(entry) = self.last_entry().cloned() else {
            return Ok(None);
        };
        let current = Snapshot::of(meta::load_for(paths, &entry.cwd, &entry.session_id));
        let (status, note, decided_at) = match entry.kind {
            Kind::Status => (entry.before.status, current.note, entry.before.decided_at.clone()),
            Kind::Note => (current.status, entry.before.note.clone(), current.decided_at),
        };
        meta::restore(paths, &entry.cwd, &entry.session_id, status, note, decided_at)?;

        let cut = self
            .lines
            .iter()
            .rposition(|l| matches!(l, Line::Decision(_)))
            .unwrap_or(0);
        self.lines.truncate(cut);
        if let Err(e) = self.rewrite() {
            self.warning = Some(format!("journal non réécrit : {e}"));
        }

        if entry.kind == Kind::Status {
            self.log_decision(&DecisionRecord {
                session_id: &entry.session_id,
                event: "undo",
                category: entry.after.status,
                at: chrono::Local::now().to_rfc3339(),
                snapshot_version: None,
                snapshot_hash: None,
                snapshot_stored: None,
                transcript_bytes: None,
            });
        }
        Ok(Some(entry.session_id))
    }

    fn rewrite(&self) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension(format!("jsonl.{}.tmp", std::process::id()));
        {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .mode(0o600)
                .open(&tmp)?;
            for line in &self.lines {
                writeln!(f, "{}", serde_json::to_string(line).unwrap())?;
            }
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox(name: &str) -> (Paths, PathBuf) {
        let root = std::env::temp_dir().join(format!("cs_journal_{name}"));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        (Paths::sandbox(&root), root)
    }

    fn shown() -> Presented {
        Presented {
            snapshot_hash: "abc".into(),
            snapshot_stored: true,
            transcript_bytes: 42,
        }
    }

    const CWD: &str = "/absent";

    #[test]
    fn undo_restores_status_and_decided_at_only() {
        let (p, root) = sandbox("status");
        let first = meta::set_status(&p, CWD, "s", Status::Hold).unwrap();
        let mut j = Journal::open(&p);
        j.decide_status(&p, CWD, "s", Status::Done, &shown()).unwrap();
        assert!(matches!(j.plan_undo(&p), UndoPlan::Clean(_)));
        // note écrite ensuite par une autre session : elle doit survivre
        meta::set_note(&p, CWD, "s", Some("récente".into())).unwrap();
        assert_eq!(j.apply_undo(&p).unwrap().as_deref(), Some("s"));
        let m = meta::load_for(&p, CWD, "s").unwrap();
        assert_eq!(m.status, Status::Hold);
        assert_eq!(m.decided_at, first.decided_at);
        assert_eq!(m.note.as_deref(), Some("récente"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn undo_of_note_restores_note_only() {
        let (p, root) = sandbox("note");
        meta::set_note(&p, CWD, "s", Some("avant".into())).unwrap();
        let mut j = Journal::open(&p);
        j.decide_note(&p, CWD, "s", Some("après".into())).unwrap();
        meta::set_status(&p, CWD, "s", Status::ReadyToBurn).unwrap();
        assert!(matches!(j.plan_undo(&p), UndoPlan::Clean(_)));
        j.apply_undo(&p).unwrap();
        let m = meta::load_for(&p, CWD, "s").unwrap();
        assert_eq!(m.note.as_deref(), Some("avant"));
        assert_eq!(m.status, Status::ReadyToBurn);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn outside_note_change_on_note_undo_is_a_conflict() {
        let (p, root) = sandbox("conflict_note");
        let mut j = Journal::open(&p);
        j.decide_note(&p, CWD, "s", Some("revue".into())).unwrap();
        meta::set_note(&p, CWD, "s", Some("autre session".into())).unwrap();
        match j.plan_undo(&p) {
            UndoPlan::Conflict { entry, current } => {
                assert_eq!(entry.after.note.as_deref(), Some("revue"));
                assert_eq!(current.note.as_deref(), Some("autre session"));
            }
            other => panic!("{other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn outside_status_change_is_a_conflict() {
        let (p, root) = sandbox("conflict_status");
        let mut j = Journal::open(&p);
        j.decide_status(&p, CWD, "s", Status::Hold, &shown()).unwrap();
        meta::set_status(&p, CWD, "s", Status::Done).unwrap();
        assert!(matches!(j.plan_undo(&p), UndoPlan::Conflict { .. }));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn undo_chain_reaches_first_decision_then_empty() {
        let (p, root) = sandbox("chain");
        let mut j = Journal::open(&p);
        for sid in ["a", "b", "c"] {
            j.decide_status(&p, CWD, sid, Status::Hold, &shown()).unwrap();
        }
        let order: Vec<_> = (0..3).map(|_| j.apply_undo(&p).unwrap().unwrap()).collect();
        assert_eq!(order, ["c", "b", "a"]);
        assert_eq!(j.plan_undo(&p), UndoPlan::Empty);
        assert_eq!(j.apply_undo(&p).unwrap(), None);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn undo_crosses_round_end() {
        let (p, root) = sandbox("round_end");
        let mut j = Journal::open(&p);
        j.decide_status(&p, CWD, "a", Status::Done, &shown()).unwrap();
        j.record(Line::RoundEnd).unwrap();
        assert_eq!(j.round_done(), 0);
        assert_eq!(j.apply_undo(&p).unwrap().as_deref(), Some("a"));
        assert!(j.lines.is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn undecided_session_returns_to_queue_after_undo() {
        let (p, root) = sandbox("undecided");
        let mut j = Journal::open(&p);
        j.decide_status(&p, CWD, "s", Status::Hold, &shown()).unwrap();
        j.apply_undo(&p).unwrap();
        let m = meta::load_for(&p, CWD, "s").unwrap();
        assert_eq!(m.status, Status::Active);
        assert_eq!(m.decided_at, None);
        assert_eq!(crate::queue::decision_after_ms(&m), None);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn round_done_ignores_notes_and_resets_after_round_end() {
        let (p, root) = sandbox("count");
        let mut j = Journal::open(&p);
        j.decide_status(&p, CWD, "a", Status::Hold, &shown()).unwrap();
        j.decide_note(&p, CWD, "a", Some("n".into())).unwrap();
        assert_eq!(j.round_done(), 1);
        j.record(Line::RoundEnd).unwrap();
        j.decide_status(&p, CWD, "b", Status::Hold, &shown()).unwrap();
        j.decide_status(&p, CWD, "c", Status::Hold, &shown()).unwrap();
        assert_eq!(j.round_done(), 2);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn decision_log_failure_does_not_fail_the_decision() {
        let (p, root) = sandbox("log_fail");
        let mut j = Journal::open(&p);
        // un dossier à la place du fichier : l'ajout échoue
        std::fs::create_dir_all(p.state_dir.join("review-decisions.jsonl")).unwrap();
        j.decide_status(&p, CWD, "s", Status::Hold, &shown()).unwrap();
        assert!(j.warning.is_some());
        assert_eq!(meta::load_for(&p, CWD, "s").unwrap().status, Status::Hold);
        assert_eq!(j.round_done(), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn journal_survives_reopen() {
        let (p, root) = sandbox("reopen");
        let mut j = Journal::open(&p);
        j.decide_status(&p, CWD, "a", Status::Hold, &shown()).unwrap();
        j.apply_undo(&p).unwrap();
        j.decide_status(&p, CWD, "b", Status::Done, &shown()).unwrap();
        let again = Journal::open(&p);
        assert_eq!(again.round_done(), 1);
        assert_eq!(again.last_entry().unwrap().session_id, "b");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn decision_log_is_append_only_and_has_no_content() {
        let (p, root) = sandbox("log");
        let mut j = Journal::open(&p);
        j.decide_status(&p, CWD, "s", Status::ReadyToBurn, &shown()).unwrap();
        j.apply_undo(&p).unwrap();
        let log = std::fs::read_to_string(p.state_dir.join("review-decisions.jsonl")).unwrap();
        let lines: Vec<serde_json::Value> =
            log.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "decision");
        assert_eq!(lines[0]["sessionId"], "s");
        assert_eq!(lines[0]["category"], "ready_to_burn");
        assert_eq!(lines[0]["snapshotVersion"], snapshot::VERSION);
        assert_eq!(lines[0]["snapshotHash"], "abc");
        assert_eq!(lines[0]["snapshotStored"], true);
        assert_eq!(lines[0]["transcriptBytes"], 42);
        assert!(lines[0]["at"].as_str().unwrap().contains('T'));
        assert_eq!(lines[1]["event"], "undo");
        std::fs::remove_dir_all(&root).ok();
    }
}
