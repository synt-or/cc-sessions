//! La revue, sans terminal : file, décisions, annulation, progression.
use crate::journal::{self, Journal, Kind, Snapshot, UndoPlan};
use crate::model::{SessionMeta, SessionRow, Status};
use crate::paths::Paths;
use crate::snapshot::{self, Frozen};
use crate::transcript::{self, SessionDetail};
use crate::{live, meta, queue};
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    Tab,
}

/// La zone que ↑↓ fait défiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    Recaps,
    Exchanges,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Browse,
    Confirm(Status),
    EditNote(String),
    ConfirmUndo { kind: Kind, theirs: Snapshot, ours: Snapshot },
    ConfirmResume,
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Exit,
    Resume { sid: String, cwd: String },
}

/// Ce qui s'est passé depuis le flag d'une session reprise.
#[derive(Debug, Clone, PartialEq)]
pub struct Banner {
    pub status: Status,
    pub flag_ms: i64,
    pub turns: u32,
    pub worked_ms: u64,
    pub last_real_ms: Option<i64>,
}

/// Date du flag telle que l'utilisateur l'a posé (sans la minute de marge de
/// `decision_after_ms` pour les anciennes entrées).
fn flag_ms(meta: &SessionMeta) -> Option<i64> {
    if let Some(at) = &meta.decided_at {
        return DateTime::parse_from_rfc3339(at).ok().map(|d| d.timestamp_millis());
    }
    let naive = NaiveDateTime::parse_from_str(&meta.updated_at, "%Y-%m-%dT%H:%M").ok()?;
    Some(Local.from_local_datetime(&naive).earliest()?.timestamp_millis())
}

pub fn banner(meta: &SessionMeta, detail: &SessionDetail) -> Option<Banner> {
    let after = queue::decision_after_ms(meta)?;
    Some(Banner {
        status: meta.status,
        flag_ms: flag_ms(meta)?,
        turns: detail.real_at_ms.iter().filter(|ms| **ms > after).count() as u32,
        worked_ms: detail.turns.iter().filter(|(at, _)| *at > after).map(|(_, ms)| ms).sum(),
        last_real_ms: detail.real_at_ms.last().copied(),
    })
}

/// Une session déplacée d'un dossier à l'autre existe en deux copies, sous le même
/// identifiant : seule la plus récente est présentée.
fn one_copy_per_session(rows: Vec<SessionRow>) -> Vec<SessionRow> {
    let mut best: HashMap<String, SessionRow> = HashMap::new();
    for row in rows {
        let rank = |r: &SessionRow| (r.info.activity_ns(), r.info.size);
        match best.get(&row.info.session_id) {
            Some(kept) if rank(kept) >= rank(&row) => {}
            _ => {
                best.insert(row.info.session_id.clone(), row);
            }
        }
    }
    best.into_values().collect()
}

pub struct Review {
    paths: Paths,
    rows: HashMap<String, SessionRow>,
    queue: Vec<String>,
    pos: usize,
    journal: Journal,
    pub mode: Mode,
    pub open: HashSet<String>,
    pub zone: Zone,
    /// Lignes défilées depuis le haut des recaps.
    pub recaps_scroll: u16,
    /// Lignes remontées depuis la fin des échanges : le dernier message d'abord.
    pub exchanges_up: u16,
    /// Bornes du défilement, connues seulement au dessin (retour à la ligne).
    pub scroll_limits: Cell<(u16, u16)>,
    pub message: Option<String>,
    /// Tout ce que l'écran montre de la session courante : jamais recalculé.
    frozen: Option<Frozen>,
    /// Instantané d'ouverture de chaque session, le seul que les modèles verront :
    /// une note écrite pendant la revue contient le raisonnement de la décision.
    /// Revenir sur une session ne le remplace pas.
    openings: HashMap<String, Frozen>,
}

impl Review {
    /// `rows` : toutes les sessions du périmètre. La file s'en déduit, et
    /// l'annulation peut y retrouver une session déjà triée.
    pub fn new(paths: Paths, rows: Vec<SessionRow>, open: HashSet<String>) -> Review {
        let journal = Journal::open(&paths);
        let rows = one_copy_per_session(rows);
        let queue: Vec<String> = queue::build(rows.clone())
            .into_iter()
            .map(|r| r.info.session_id)
            .collect();
        let mode = if queue.is_empty() { Mode::Finished } else { Mode::Browse };
        let mut journal = journal;
        // File vidée hors revue (`cs done`) : le tour est fini, le compteur repart.
        if queue.is_empty() && journal.round_done() > 0 {
            let _ = journal.record(journal::Line::RoundEnd);
        }
        let mut review = Review {
            paths,
            rows: rows.into_iter().map(|r| (r.info.session_id.clone(), r)).collect(),
            queue,
            pos: 0,
            journal,
            mode,
            open,
            zone: Zone::Exchanges,
            recaps_scroll: 0,
            exchanges_up: 0,
            scroll_limits: Cell::new((u16::MAX, u16::MAX)),
            message: None,
            frozen: None,
            openings: HashMap::new(),
        };
        review.load_detail();
        review
    }

    /// `(faites, total)` : le total se déduit de ce qui reste, il grandit donc
    /// seul si une session reprise entre dans la file.
    pub fn progress(&self) -> (u32, u32) {
        let done = self.journal.round_done();
        (done, done + self.queue.len() as u32)
    }

    pub fn current(&self) -> Option<&SessionRow> {
        self.rows.get(self.queue.get(self.pos)?)
    }

    pub fn frozen(&self) -> Option<&Frozen> {
        self.frozen.as_ref()
    }

    /// Alerte qui reste affichée tant qu'elle s'applique à la session courante. Elle
    /// porte sur l'ouverture, seul instantané que désignera la décision.
    pub fn alert(&self) -> Option<&'static str> {
        let frozen = self.opening()?;
        if frozen.read_error {
            Some("⚠ Transcript illisible : décision refusée, s pour sauter.")
        } else if !frozen.stored {
            Some("⚠ Instantané non écrit : une décision ne sera pas exploitable pour le banc.")
        } else {
            None
        }
    }

    fn opening(&self) -> Option<&Frozen> {
        self.openings.get(&self.current()?.info.session_id)
    }

    pub fn is_open(&self) -> bool {
        self.current().is_some_and(|r| self.open.contains(&r.info.session_id))
    }

    /// Fige la session qui s'ouvre : c'est le seul moment où le transcript est lu.
    fn load_detail(&mut self) {
        let Some(mut row) = self.current().cloned() else {
            self.frozen = None;
            return;
        };
        let sid = row.info.session_id.clone();
        if self.frozen.as_ref().is_some_and(|f| f.session_id == sid) {
            return;
        }
        self.recaps_scroll = 0;
        self.exchanges_up = 0;
        self.scroll_limits.set((u16::MAX, u16::MAX));
        // Déjà ouverte : même contenu que la première fois, seule la note suit. Ce que
        // l'utilisateur revoit reste ce que les modèles verront. Seul un échec de lecture ou
        // d'écriture se retente : un instantané vide ne peut pas servir de référence.
        row.meta = meta::load_for(&self.paths, &Self::cwd(&row), &sid).or(row.meta);
        if let Some(opening) = self.openings.get_mut(&sid) {
            if !opening.read_error {
                if !opening.stored {
                    let _ = opening.store(&self.paths);
                }
                let note = row.meta.as_ref().and_then(|m| m.note.as_deref());
                self.frozen = Some(opening.with_note(note));
                self.rows.insert(sid, row);
                return;
            }
        }
        // Tout l'instantané décrit ce même instant, même si la revue tourne depuis
        // longtemps : note et statut relus, taille et date prises sur le transcript.
        if let Ok(file) = row.info.path.metadata() {
            row.info.size = file.len();
        }
        let (detail, read_error) = match transcript::read_detail(&row.info.path) {
            Ok(detail) => (detail, false),
            Err(_) => (SessionDetail::default(), true),
        };
        if let Some(last) = detail.real_at_ms.last() {
            row.info.digest.last_real_at_ms = Some(*last);
        }
        let shown = row.meta.as_ref().and_then(|m| banner(m, &detail));
        let mut frozen = snapshot::freeze(&row, &detail, shown.as_ref(), self.is_open(), read_error);
        if let Err(e) = frozen.store(&self.paths) {
            self.message = Some(format!("instantané non écrit : {e}"));
        }
        self.openings.insert(sid.clone(), frozen.clone());
        self.frozen = Some(frozen);
        self.rows.insert(sid, row);
    }

    fn after_move(&mut self) {
        if self.queue.is_empty() {
            self.pos = 0;
        } else {
            self.pos %= self.queue.len();
        }
        self.load_detail();
    }

    fn cwd(row: &SessionRow) -> String {
        row.info.cwd.clone().unwrap_or_default()
    }

    fn decide(&mut self, status: Status) {
        let Some(row) = self.current().cloned() else { return };
        let sid = row.info.session_id.clone();
        let Some(opening) = self.openings.get(&sid) else { return };
        if opening.read_error {
            self.message = Some("Transcript illisible : décision refusée.".into());
            return;
        }
        let shown = journal::Presented {
            snapshot_hash: opening.hash.clone(),
            snapshot_stored: opening.stored,
            transcript_bytes: opening.transcript_bytes,
        };
        match self.journal.decide_status(&self.paths, &Self::cwd(&row), &sid, status, &shown) {
            Ok(written) => {
                if let Some(r) = self.rows.get_mut(&sid) {
                    r.meta = Some(written);
                }
                self.queue.remove(self.pos);
                self.message = None;
                if self.queue.is_empty() {
                    let (done, _) = self.progress();
                    if let Err(e) = self.journal.record(journal::Line::RoundEnd) {
                        self.message = Some(format!("journal : {e}"));
                    } else {
                        self.message = Some(format!("{done} sessions traitées. `cs -z` annule la dernière."));
                    }
                    self.mode = Mode::Finished;
                }
                self.after_move();
                if let Some(warning) = self.journal.warning.take() {
                    self.message = Some(warning);
                }
            }
            Err(e) => self.message = Some(format!("écriture impossible : {e}")),
        }
    }

    fn save_note(&mut self, text: String) {
        let Some(row) = self.current().cloned() else { return };
        let sid = row.info.session_id.clone();
        let note = (!text.trim().is_empty()).then(|| text.trim().to_string());
        match self.journal.decide_note(&self.paths, &Self::cwd(&row), &sid, note) {
            Ok(written) => {
                if let Some(frozen) = &self.frozen {
                    // L'écran suit la note. Le fichier d'ouverture ne change pas, et la
                    // décision continuera de le désigner.
                    self.frozen = Some(frozen.with_note(written.note.as_deref()));
                }
                if let Some(r) = self.rows.get_mut(&sid) {
                    r.meta = Some(written);
                }
            }
            Err(e) => self.message = Some(format!("écriture impossible : {e}")),
        }
    }

    /// `z`, et le lancement par `cs -z`.
    pub fn undo(&mut self) {
        match self.journal.plan_undo(&self.paths) {
            UndoPlan::Empty => self.message = Some("Aucune décision à annuler.".into()),
            UndoPlan::Clean(_) => self.apply_undo(),
            UndoPlan::Conflict { entry, current } => {
                self.mode = Mode::ConfirmUndo {
                    kind: entry.kind,
                    theirs: current,
                    ours: entry.before,
                };
            }
        }
    }

    fn apply_undo(&mut self) {
        let sid = match self.journal.apply_undo(&self.paths) {
            Ok(Some(sid)) => sid,
            Ok(None) => return,
            Err(e) => {
                self.message = Some(format!("annulation impossible : {e}"));
                self.mode = if self.queue.is_empty() { Mode::Finished } else { Mode::Browse };
                return;
            }
        };
        self.message = None;
        let Some(row) = self.rows.get_mut(&sid) else {
            self.message = Some(format!("Annulé, mais {sid} est hors de cette liste."));
            self.mode = if self.queue.is_empty() { Mode::Finished } else { Mode::Browse };
            return;
        };
        let cwd = Self::cwd(row);
        row.meta = meta::load_for(&self.paths, &cwd, &sid);
        let back_in_queue = queue::needs_review(row);
        self.queue.retain(|s| *s != sid);
        // Une annulation de note peut viser une session déjà triée : elle ne
        // revient pas forcément dans la file.
        if back_in_queue {
            let at = self.pos.min(self.queue.len());
            self.queue.insert(at, sid);
            self.pos = at;
        }
        self.mode = if self.queue.is_empty() { Mode::Finished } else { Mode::Browse };
        self.frozen = None;
        self.after_move();
        if let Some(warning) = self.journal.warning.take() {
            self.message = Some(warning);
        }
    }

    /// Un collage n'écrit que dans la note, sans jamais agir comme des touches.
    pub fn on_paste(&mut self, pasted: &str) {
        if let Mode::EditNote(text) = &mut self.mode {
            text.push_str(&pasted.split_whitespace().collect::<Vec<_>>().join(" "));
        }
    }

    pub fn on_key(&mut self, key: Key) -> Outcome {
        match self.mode.clone() {
            Mode::Browse => return self.on_browse(key),
            Mode::Confirm(status) => {
                self.mode = Mode::Browse;
                if matches!(key, Key::Char('o') | Key::Enter) {
                    self.decide(status);
                }
            }
            Mode::EditNote(mut text) => match key {
                Key::Enter => {
                    self.mode = Mode::Browse;
                    self.save_note(text);
                }
                Key::Esc => self.mode = Mode::Browse,
                Key::Backspace => {
                    text.pop();
                    self.mode = Mode::EditNote(text);
                }
                Key::Char(c) => {
                    text.push(c);
                    self.mode = Mode::EditNote(text);
                }
                _ => {}
            },
            Mode::ConfirmUndo { .. } => {
                if matches!(key, Key::Char('o') | Key::Enter) {
                    self.apply_undo();
                } else {
                    self.mode = if self.queue.is_empty() { Mode::Finished } else { Mode::Browse };
                }
            }
            Mode::ConfirmResume => {
                self.mode = Mode::Browse;
                if matches!(key, Key::Char('o') | Key::Enter) {
                    return self.resume();
                }
            }
            Mode::Finished => match key {
                Key::Char('z') => self.undo(),
                _ => return Outcome::Exit,
            },
        }
        Outcome::Continue
    }

    fn resume(&self) -> Outcome {
        match self.current() {
            Some(row) => Outcome::Resume {
                sid: row.info.session_id.clone(),
                cwd: Self::cwd(row),
            },
            None => Outcome::Continue,
        }
    }

    fn scroll(&mut self, up: bool) {
        let (recaps_max, exchanges_max) = self.scroll_limits.get();
        match (self.zone, up) {
            (Zone::Recaps, true) => self.recaps_scroll = self.recaps_scroll.saturating_sub(1),
            (Zone::Recaps, false) => self.recaps_scroll = (self.recaps_scroll + 1).min(recaps_max),
            (Zone::Exchanges, true) => self.exchanges_up = (self.exchanges_up + 1).min(exchanges_max),
            (Zone::Exchanges, false) => self.exchanges_up = self.exchanges_up.saturating_sub(1),
        }
    }

    fn on_browse(&mut self, key: Key) -> Outcome {
        self.message = None;
        let confirm = |status| Mode::Confirm(status);
        match key {
            Key::Char('d') => self.mode = confirm(Status::Done),
            Key::Char('h') => self.mode = confirm(Status::Hold),
            Key::Char('b') => self.mode = confirm(Status::ReadyToBurn),
            Key::Char('m') => self.mode = confirm(Status::NeedsManualWork),
            Key::Char('a') => self.mode = confirm(Status::Active),
            Key::Char('n') => {
                // Relue : une autre session a pu écrire depuis l'ouverture.
                let note = self.current().and_then(|r| {
                    meta::load_for(&self.paths, &Self::cwd(r), &r.info.session_id)
                        .or_else(|| r.meta.clone())
                        .and_then(|m| m.note)
                });
                self.mode = Mode::EditNote(note.unwrap_or_default());
            }
            Key::Char('r') => {
                // Elle a pu être ouverte depuis le lancement de la revue.
                self.open.extend(live::open_sessions(&self.paths));
                if self.is_open() {
                    self.mode = Mode::ConfirmResume;
                } else {
                    return self.resume();
                }
            }
            Key::Char('s') => {
                if self.pos < self.queue.len() {
                    let sid = self.queue.remove(self.pos);
                    self.queue.push(sid);
                    self.after_move();
                }
            }
            Key::Char('z') => self.undo(),
            Key::Char('q') => return Outcome::Exit,
            Key::Tab => {
                self.zone = match self.zone {
                    Zone::Recaps => Zone::Exchanges,
                    Zone::Exchanges => Zone::Recaps,
                };
            }
            Key::Up | Key::Down => self.scroll(key == Key::Up),
            Key::Left => {
                if !self.queue.is_empty() {
                    self.pos = (self.pos + self.queue.len() - 1) % self.queue.len();
                    self.after_move();
                }
            }
            Key::Right => {
                self.pos += 1;
                self.after_move();
            }
            _ => {}
        }
        Outcome::Continue
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::queue::tests::row;
    use std::path::PathBuf;

    const NOW_LIKE: i64 = 4_000_000_000_000;

    pub fn sandbox(name: &str) -> (Paths, PathBuf) {
        let root = std::env::temp_dir().join(format!("cs_review_{name}"));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        (Paths::sandbox(&root), root)
    }

    /// La date est dans le futur pour rester postérieure à toute décision de test.
    fn rows(n: usize) -> Vec<SessionRow> {
        (0..n)
            .map(|i| {
                let mut r = row(&format!("s{i}"), Some(NOW_LIKE - i as i64), None);
                r.info.path = empty_transcript();
                r
            })
            .collect()
    }

    fn empty_transcript() -> PathBuf {
        let path = std::env::temp_dir().join("cs_review_empty.jsonl");
        std::fs::write(&path, "").unwrap();
        path
    }

    fn press(r: &mut Review, keys: &[Key]) -> Outcome {
        keys.iter().fold(Outcome::Continue, |_, k| r.on_key(*k))
    }

    fn current(r: &Review) -> String {
        r.current().unwrap().info.session_id.clone()
    }

    const HOLD_OK: [Key; 2] = [Key::Char('h'), Key::Char('o')];

    /// Sessions à des dates passées : une décision les sort durablement.
    fn past_rows(n: usize) -> Vec<SessionRow> {
        (0..n)
            .map(|i| {
                let mut r = row(&format!("s{i}"), Some(1000 - i as i64), None);
                r.info.path = empty_transcript();
                r
            })
            .collect()
    }

    #[test]
    fn decide_asks_confirmation_then_advances() {
        let (p, root) = sandbox("confirm");
        let mut r = Review::new(p.clone(), past_rows(3), HashSet::new());
        assert_eq!(r.progress(), (0, 3));
        r.on_key(Key::Char('h'));
        assert_eq!(r.mode, Mode::Confirm(Status::Hold));
        assert!(meta::load_for(&p, "/Users/x/repo", "s0").is_none(), "rien avant confirmation");
        r.on_key(Key::Char('o'));
        assert_eq!(r.mode, Mode::Browse);
        assert_eq!(current(&r), "s1");
        assert_eq!(r.progress(), (1, 3));
        assert_eq!(meta::load_for(&p, "/Users/x/repo", "s0").unwrap().status, Status::Hold);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn any_other_key_cancels_confirmation() {
        let (p, root) = sandbox("cancel");
        let mut r = Review::new(p.clone(), past_rows(2), HashSet::new());
        press(&mut r, &[Key::Char('d'), Key::Char('x')]);
        assert_eq!(r.mode, Mode::Browse);
        assert_eq!(current(&r), "s0");
        assert!(meta::load_for(&p, "/Users/x/repo", "s0").is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn enter_also_confirms() {
        let (p, root) = sandbox("enter");
        let mut r = Review::new(p, past_rows(2), HashSet::new());
        press(&mut r, &[Key::Char('d'), Key::Enter]);
        assert_eq!(current(&r), "s1");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn keep_marks_active_with_decided_at_and_leaves_queue() {
        let (p, root) = sandbox("keep");
        let mut r = Review::new(p.clone(), past_rows(2), HashSet::new());
        press(&mut r, &[Key::Char('a'), Key::Char('o')]);
        let m = meta::load_for(&p, "/Users/x/repo", "s0").unwrap();
        assert_eq!(m.status, Status::Active);
        assert!(m.decided_at.is_some());
        assert_eq!(current(&r), "s1");
        // La session reste suivie : elle sort de la file, pas de cs.
        let again = Review::new(p, past_rows(2), HashSet::new());
        assert_eq!(again.rows.len(), 2);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn z_chain_walks_back_to_first_decision() {
        let (p, root) = sandbox("chain");
        let mut r = Review::new(p, past_rows(4), HashSet::new());
        for _ in 0..3 {
            press(&mut r, &HOLD_OK);
        }
        assert_eq!(current(&r), "s3");
        assert_eq!(r.progress(), (3, 4));
        r.on_key(Key::Char('z'));
        assert_eq!(current(&r), "s2");
        r.on_key(Key::Char('z'));
        assert_eq!(current(&r), "s1");
        r.on_key(Key::Char('z'));
        assert_eq!(current(&r), "s0");
        assert_eq!(r.progress(), (0, 4));
        r.on_key(Key::Char('z'));
        assert_eq!(r.message.as_deref(), Some("Aucune décision à annuler."));
        assert_eq!(current(&r), "s0");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn skip_moves_to_end_without_progress() {
        let (p, root) = sandbox("skip");
        let mut r = Review::new(p, past_rows(3), HashSet::new());
        r.on_key(Key::Char('s'));
        assert_eq!(current(&r), "s1");
        assert_eq!(r.progress(), (0, 3));
        r.on_key(Key::Char('s'));
        r.on_key(Key::Char('s'));
        assert_eq!(current(&r), "s0");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn tab_switches_the_scrolled_zone() {
        let (p, root) = sandbox("zones");
        let mut r = Review::new(p, past_rows(1), HashSet::new());
        assert_eq!(r.zone, Zone::Exchanges);
        r.on_key(Key::Up);
        assert_eq!(r.exchanges_up, 1);
        r.on_key(Key::Down);
        r.on_key(Key::Down);
        assert_eq!(r.exchanges_up, 0);
        r.on_key(Key::Tab);
        r.on_key(Key::Down);
        assert_eq!((r.recaps_scroll, r.exchanges_up), (1, 0));
        r.scroll_limits.set((1, 0));
        r.on_key(Key::Down);
        assert_eq!(r.recaps_scroll, 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn arrows_navigate_without_deciding() {
        let (p, root) = sandbox("arrows");
        let mut r = Review::new(p, past_rows(3), HashSet::new());
        r.on_key(Key::Left);
        assert_eq!(current(&r), "s2");
        r.on_key(Key::Right);
        assert_eq!(current(&r), "s0");
        assert_eq!(r.progress(), (0, 3));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn note_edit_is_prefilled_and_stays_on_session() {
        let (p, root) = sandbox("note");
        meta::set_note(&p, "/Users/x/repo", "s0", Some("avant".into())).unwrap();
        let mut rows = past_rows(2);
        rows[0].meta = meta::load_for(&p, "/Users/x/repo", "s0");
        let mut r = Review::new(p.clone(), rows, HashSet::new());
        r.on_key(Key::Char('n'));
        assert_eq!(r.mode, Mode::EditNote("avant".into()));
        press(&mut r, &[Key::Char('!'), Key::Enter]);
        assert_eq!(current(&r), "s0");
        assert_eq!(meta::load_for(&p, "/Users/x/repo", "s0").unwrap().note.as_deref(), Some("avant!"));
        assert_eq!(r.progress(), (0, 2));
        // la note s'annule comme le reste
        r.on_key(Key::Char('z'));
        assert_eq!(meta::load_for(&p, "/Users/x/repo", "s0").unwrap().note.as_deref(), Some("avant"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn paste_never_acts_as_keys() {
        let (p, root) = sandbox("paste");
        let mut r = Review::new(p.clone(), past_rows(2), HashSet::new());
        r.on_paste("d\no");
        assert_eq!(r.mode, Mode::Browse);
        r.on_key(Key::Char('n'));
        r.on_paste("ligne\ndo");
        assert_eq!(r.mode, Mode::EditNote("ligne do".into()));
        assert_eq!(current(&r), "s0");
        assert!(meta::load_for(&p, "/Users/x/repo", "s0").is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    /// Session triée avant `decidedAt`, reprise depuis.
    fn legacy_row(p: &Paths) -> SessionRow {
        std::fs::create_dir_all(&p.state_dir).unwrap();
        std::fs::write(
            meta::central_notes_file(p),
            "{\"sessionId\":\"old\",\"updatedAt\":\"2026-10-12T14:32\",\"status\":\"hold\"}\n",
        )
        .unwrap();
        let last = DateTime::parse_from_rfc3339("2026-10-15T09:40:00+02:00").unwrap().timestamp_millis();
        let mut legacy = row("old", Some(last), meta::load_for(p, "/Users/x/repo", "old"));
        legacy.info.path = empty_transcript();
        legacy
    }

    #[test]
    fn note_on_legacy_session_keeps_it_in_queue() {
        let (p, root) = sandbox("legacy_note");
        let mut r = Review::new(p.clone(), vec![legacy_row(&p)], HashSet::new());
        press(&mut r, &[Key::Char('n'), Key::Char('x'), Key::Enter]);
        let mut again = legacy_row_after(&p);
        again.meta = meta::load_for(&p, "/Users/x/repo", "old");
        assert!(queue::needs_review(&again), "la session a quitté la file sans décision");
        std::fs::remove_dir_all(&root).ok();
    }

    fn legacy_row_after(_p: &Paths) -> SessionRow {
        let last = DateTime::parse_from_rfc3339("2026-10-15T09:40:00+02:00").unwrap().timestamp_millis();
        row("old", Some(last), None)
    }

    #[test]
    fn undo_on_legacy_session_returns_to_it() {
        let (p, root) = sandbox("legacy_undo");
        let mut r = Review::new(p.clone(), vec![legacy_row(&p), past_rows(1).remove(0)], HashSet::new());
        assert_eq!(current(&r), "old");
        press(&mut r, &[Key::Char('d'), Key::Char('o')]);
        assert_eq!(current(&r), "s0");
        r.on_key(Key::Char('z'));
        assert_eq!(current(&r), "old");
        assert_eq!(r.progress(), (0, 2));
        let m = meta::load_for(&p, "/Users/x/repo", "old").unwrap();
        assert_eq!(m.status, Status::Hold);
        assert!(m.decided_at.unwrap().starts_with("2026-10-12T14:32:59"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn queue_emptied_outside_review_resets_counter() {
        let (p, root) = sandbox("outside");
        let mut r = Review::new(p.clone(), past_rows(2), HashSet::new());
        press(&mut r, &HOLD_OK);
        meta::set_status(&p, "/Users/x/repo", "s1", Status::Done).unwrap();
        let reload = |p: &Paths, n: usize| {
            let mut v = past_rows(n);
            for row in &mut v {
                row.meta = meta::load_for(p, "/Users/x/repo", &row.info.session_id);
            }
            v
        };
        let done = Review::new(p.clone(), reload(&p, 2), HashSet::new());
        assert_eq!(done.progress(), (0, 0));
        let fresh = Review::new(p.clone(), reload(&p, 3), HashSet::new());
        assert_eq!(fresh.progress(), (0, 1));
        std::fs::remove_dir_all(&root).ok();
    }

    fn write_transcript(root: &std::path::Path, text: &str) -> PathBuf {
        let path = root.join("t.jsonl");
        let line = format!("{{\"type\":\"user\",\"timestamp\":\"2026-10-15T15:00:00.000Z\",\"origin\":{{\"kind\":\"human\"}},\"message\":{{\"content\":\"{text}\"}}}}\n");
        std::fs::write(&path, line).unwrap();
        path
    }

    #[test]
    fn opening_a_session_freezes_it_and_decision_references_the_file() {
        let (p, root) = sandbox("frozen");
        let mut rows = past_rows(2);
        rows[0].info.path = write_transcript(&root, "avant");
        let mut r = Review::new(p.clone(), rows, HashSet::new());
        let hash = r.frozen().unwrap().hash.clone();
        let file = p.snapshots.join("s0").join(format!("{hash}.txt"));
        assert!(std::fs::read_to_string(&file).unwrap().contains("avant"));
        press(&mut r, &HOLD_OK);
        let log = std::fs::read_to_string(p.state_dir.join("review-decisions.jsonl")).unwrap();
        let line: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        assert_eq!(line["snapshotHash"], hash.as_str());
        assert_eq!(line["snapshotStored"], true);
        assert!(!log.contains("avant"), "pas de contenu dans l'historique");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn transcript_changing_after_opening_does_not_change_what_is_shown() {
        let (p, root) = sandbox("stable");
        let mut rows = past_rows(1);
        rows[0].info.path = write_transcript(&root, "figé");
        let path = rows[0].info.path.clone();
        let mut r = Review::new(p, rows, HashSet::new());
        let hash = r.frozen().unwrap().hash.clone();
        write_transcript(&root, "changé");
        assert_eq!(path, root.join("t.jsonl"));
        r.on_key(Key::Up);
        r.on_key(Key::Char('x'));
        assert_eq!(r.frozen().unwrap().hash, hash);
        assert!(r.frozen().unwrap().text().contains("figé"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn coming_back_shows_the_opening_even_if_the_transcript_grew() {
        let (p, root) = sandbox("revisit");
        let mut rows = past_rows(2);
        rows[0].info.path = write_transcript(&root, "ouverture");
        let path = rows[0].info.path.clone();
        let mut r = Review::new(p.clone(), rows, HashSet::new());
        let opening = r.frozen().unwrap().hash.clone();
        let bytes = r.frozen().unwrap().transcript_bytes;
        r.on_key(Key::Right);
        let later = "{\"type\":\"user\",\"timestamp\":\"2026-10-16T15:00:00.000Z\",\"origin\":{\"kind\":\"human\"},\"message\":{\"content\":\"ajouté\"}}\n";
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(later);
        std::fs::write(&path, text).unwrap();
        r.on_key(Key::Left);
        assert_eq!(current(&r), "s0");
        assert_eq!(r.frozen().unwrap().hash, opening);
        assert!(!r.frozen().unwrap().text().contains("ajouté"));
        assert_eq!(r.frozen().unwrap().transcript_bytes, bytes);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unreadable_opening_is_replaced_once_readable_and_never_designated() {
        let (p, root) = sandbox("reopen_read");
        let mut rows = past_rows(2);
        let path = root.join("t.jsonl");
        rows[0].info.path = path.clone();
        let mut r = Review::new(p.clone(), rows, HashSet::new());
        assert!(r.alert().is_some());
        let empty = r.frozen().unwrap().hash.clone();
        write_transcript(&root, "lisible");
        r.on_key(Key::Right);
        r.on_key(Key::Left);
        assert_eq!(r.alert(), None);
        let opening = r.frozen().unwrap().hash.clone();
        assert_ne!(opening, empty);
        press(&mut r, &HOLD_OK);
        let log = std::fs::read_to_string(p.state_dir.join("review-decisions.jsonl")).unwrap();
        let line: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        assert_eq!(line["snapshotHash"], opening.as_str());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unwritten_opening_keeps_its_alert_and_is_retried() {
        let (p, root) = sandbox("unwritten");
        std::fs::create_dir_all(p.snapshots.parent().unwrap()).unwrap();
        std::fs::write(&p.snapshots, "un fichier à la place du dossier").unwrap();
        let mut r = Review::new(p.clone(), past_rows(2), HashSet::new());
        assert!(r.alert().is_some());
        r.on_key(Key::Up);
        assert!(r.alert().is_some(), "l'alerte reste");
        std::fs::remove_file(&p.snapshots).unwrap();
        r.on_key(Key::Right);
        r.on_key(Key::Left);
        assert_eq!(r.alert(), None);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn note_edit_never_changes_the_snapshot_models_will_see() {
        let (p, root) = sandbox("frozen_note");
        let mut r = Review::new(p.clone(), past_rows(2), HashSet::new());
        let opening = r.frozen().unwrap().hash.clone();
        press(&mut r, &[Key::Char('n'), Key::Char('x'), Key::Enter]);
        assert_eq!(r.frozen().unwrap().note, ["x"], "l'écran suit la note");
        assert_eq!(std::fs::read_dir(p.snapshots.join("s0")).unwrap().count(), 1);
        // Partir et revenir ne remplace pas l'instantané d'ouverture.
        r.on_key(Key::Right);
        r.on_key(Key::Left);
        assert_eq!(current(&r), "s0");
        press(&mut r, &HOLD_OK);
        let log = std::fs::read_to_string(p.state_dir.join("review-decisions.jsonl")).unwrap();
        let line: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
        assert_eq!(line["snapshotHash"], opening.as_str());
        assert_eq!(line["snapshotStored"], true);
        let file = p.snapshots.join("s0").join(format!("{opening}.txt"));
        assert!(!std::fs::read_to_string(file).unwrap().contains("| x"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn duplicate_session_shows_only_the_most_recent_copy() {
        let (p, root) = sandbox("dup");
        let mut old = row("s", Some(10), None);
        old.info.path = write_transcript(&root, "ancienne");
        let newer_path = root.join("n.jsonl");
        std::fs::copy(&old.info.path, &newer_path).unwrap();
        let mut new = row("s", Some(20), None);
        new.info.path = newer_path.clone();
        let r = Review::new(p, vec![old, new], HashSet::new());
        assert_eq!(r.progress(), (0, 1));
        assert_eq!(r.current().unwrap().info.path, newer_path);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn unreadable_transcript_blocks_the_decision_and_stays_flagged() {
        let (p, root) = sandbox("unreadable");
        let mut lines = past_rows(2);
        lines[0].info.path = PathBuf::from("/n/existe/pas.jsonl");
        let mut r = Review::new(p.clone(), lines, HashSet::new());
        assert!(r.frozen().unwrap().read_error);
        assert!(r.alert().is_some());
        press(&mut r, &HOLD_OK);
        assert_eq!(current(&r), "s0");
        assert_eq!(r.progress(), (0, 2));
        assert!(meta::load_for(&p, "/Users/x/repo", "s0").is_none());
        r.on_key(Key::Up);
        assert!(r.alert().is_some(), "l'alerte reste");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn snapshot_reflects_note_written_after_launch() {
        let (p, root) = sandbox("late_note");
        let mut rows = past_rows(2);
        rows[1].info.path = write_transcript(&root, "x");
        let mut r = Review::new(p.clone(), rows, HashSet::new());
        meta::set_note(&p, "/Users/x/repo", "s1", Some("écrite ailleurs".into())).unwrap();
        r.on_key(Key::Right);
        assert_eq!(r.frozen().unwrap().note, ["écrite ailleurs"]);
        r.on_key(Key::Char('n'));
        assert_eq!(r.mode, Mode::EditNote("écrite ailleurs".into()));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn esc_in_note_cancels() {
        let (p, root) = sandbox("note_esc");
        let mut r = Review::new(p.clone(), past_rows(1), HashSet::new());
        press(&mut r, &[Key::Char('n'), Key::Char('x'), Key::Esc]);
        assert_eq!(r.mode, Mode::Browse);
        assert!(meta::load_for(&p, "/Users/x/repo", "s0").is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn last_decision_finishes_and_undo_flag_reopens() {
        let (p, root) = sandbox("finish");
        let mut r = Review::new(p.clone(), past_rows(1), HashSet::new());
        press(&mut r, &HOLD_OK);
        assert_eq!(r.mode, Mode::Finished);
        assert_eq!(r.on_key(Key::Char('q')), Outcome::Exit);
        // relance avec `cs -z`
        let rows = |p: &Paths| {
            let mut v = past_rows(1);
            v[0].meta = meta::load_for(p, "/Users/x/repo", "s0");
            v
        };
        let mut again = Review::new(p.clone(), rows(&p), HashSet::new());
        assert_eq!(again.mode, Mode::Finished);
        again.undo();
        assert_eq!(again.mode, Mode::Browse);
        assert_eq!(current(&again), "s0");
        assert_eq!(again.progress(), (0, 1));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn progress_survives_restart() {
        let (p, root) = sandbox("restart");
        let mut r = Review::new(p.clone(), past_rows(3), HashSet::new());
        press(&mut r, &HOLD_OK);
        let reload = |p: &Paths| {
            let mut v = past_rows(3);
            for row in &mut v {
                row.meta = meta::load_for(p, "/Users/x/repo", &row.info.session_id);
            }
            v
        };
        let again = Review::new(p.clone(), reload(&p), HashSet::new());
        assert_eq!(again.progress(), (1, 3));
        assert_eq!(current(&again), "s1");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resume_warns_when_session_is_open() {
        let (p, root) = sandbox("resume");
        let open: HashSet<String> = ["s0".to_string()].into();
        let mut r = Review::new(p.clone(), past_rows(2), open);
        assert!(r.is_open());
        assert_eq!(r.on_key(Key::Char('r')), Outcome::Continue);
        assert_eq!(r.mode, Mode::ConfirmResume);
        assert!(matches!(r.on_key(Key::Char('o')), Outcome::Resume { sid, .. } if sid == "s0"));
        let mut closed = Review::new(p, past_rows(2), HashSet::new());
        assert!(matches!(closed.on_key(Key::Char('r')), Outcome::Resume { .. }));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn undo_conflict_asks_before_overwriting() {
        let (p, root) = sandbox("conflict");
        let mut r = Review::new(p.clone(), past_rows(2), HashSet::new());
        press(&mut r, &[Key::Char('n'), Key::Char('x'), Key::Enter]);
        meta::set_note(&p, "/Users/x/repo", "s0", Some("autre session".into())).unwrap();
        r.on_key(Key::Char('z'));
        assert!(matches!(r.mode, Mode::ConfirmUndo { .. }));
        r.on_key(Key::Char('x'));
        assert_eq!(r.mode, Mode::Browse);
        assert_eq!(meta::load_for(&p, "/Users/x/repo", "s0").unwrap().note.as_deref(), Some("autre session"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn nothing_to_review_finishes_at_once() {
        let (p, root) = sandbox("empty");
        let r = Review::new(p, vec![], HashSet::new());
        assert_eq!(r.mode, Mode::Finished);
        assert_eq!(r.progress(), (0, 0));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn resumed_session_stays_in_queue_after_decision_reload() {
        // date du dernier message dans le futur : postérieure à la décision
        let (p, root) = sandbox("future");
        let mut r = Review::new(p.clone(), rows(1), HashSet::new());
        press(&mut r, &HOLD_OK);
        let mut again = rows(1);
        again[0].meta = meta::load_for(&p, "/Users/x/repo", "s0");
        let reopened = Review::new(p, again, HashSet::new());
        assert_eq!(current(&reopened), "s0");
        std::fs::remove_dir_all(&root).ok();
    }

    fn meta_at(at: &str, status: Status) -> SessionMeta {
        SessionMeta {
            session_id: "s".into(),
            updated_at: "2026-10-12T14:32".into(),
            status,
            note: None,
            decided_at: Some(at.into()),
        }
    }

    #[test]
    fn banner_counts_turns_and_time_after_flag() {
        let at = "2026-10-12T14:32:00+02:00";
        let flag = DateTime::parse_from_rfc3339(at).unwrap().timestamp_millis();
        let detail = SessionDetail {
            real_at_ms: vec![flag - 5, flag + 1, flag + 2, flag + 3],
            turns: vec![(flag - 5, 999), (flag + 1, 1000), (flag + 3, 2000)],
            ..Default::default()
        };
        let b = banner(&meta_at(at, Status::Hold), &detail).unwrap();
        assert_eq!(b.status, Status::Hold);
        assert_eq!(b.flag_ms, flag);
        assert_eq!(b.turns, 3);
        assert_eq!(b.worked_ms, 3000);
        assert_eq!(b.last_real_ms, Some(flag + 3));
    }

    #[test]
    fn banner_absent_without_decision() {
        let mut m = meta_at("2026-10-12T14:32:00+02:00", Status::Active);
        m.decided_at = None;
        assert_eq!(banner(&m, &SessionDetail::default()), None);
    }
}
