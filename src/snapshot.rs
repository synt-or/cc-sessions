//! L'instantané figé de ce que l'utilisateur voit d'une session pendant qu'il décide.
//!
//! L'écran dessine l'instantané, jamais le transcript : la même donnée sert plus
//! tard à évaluer des classifieurs. Le fichier contient du texte de transcript en
//! clair (données sensibles possibles) : il reste local, en mode 600, hors
//! de tout dépôt et hors de l'index Spotlight.
use crate::model::{SessionRow, Status};
use crate::paths::Paths;
use crate::review::state::Banner;
use crate::transcript::SessionDetail;
use chrono::{Local, TimeZone};
use sha2::{Digest, Sha256};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

/// À incrémenter si le format du texte change.
pub const VERSION: u32 = 1;

fn formatted(ms: i64, format: &str) -> String {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|d| d.format(format).to_string())
        .unwrap_or_else(|| "?".into())
}

fn when(ms: i64) -> String {
    formatted(ms, "%d/%m à %H:%M")
}

fn short(ms: i64) -> String {
    formatted(ms, "%d/%m %H:%M")
}

pub fn status_label(status: Status) -> &'static str {
    match status {
        Status::Active => "active",
        Status::Hold => "⏳ hold",
        Status::Done => "✓ done",
        Status::ReadyToBurn => "🔥 burn",
        Status::NeedsManualWork => "🔍 manual",
    }
}

fn duration(ms: u64) -> String {
    let minutes = ms / 60_000;
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, m) => format!("{h} h {m:02}"),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExchangeLine {
    /// Ligne « qui · quand », atténuée à l'écran.
    pub is_label: bool,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Frozen {
    pub session_id: String,
    pub transcript_bytes: u64,
    pub header: Vec<String>,
    pub note: Vec<String>,
    pub recaps: Vec<String>,
    pub exchanges: Vec<ExchangeLine>,
    pub hash: String,
    pub stored: bool,
    /// Le transcript n'a pas pu être lu : l'instantané ne montre pas la session.
    pub read_error: bool,
}

const NO_NOTE: &str = "—";

/// Retire ce que le terminal n'affiche pas : un modèle ne doit pas lire un contenu
/// que l'utilisateur n'a pas pu voir à l'écran.
fn clean(text: &str) -> String {
    let invisible = |c: char| {
        c.is_control()
            || matches!(c, '\u{ad}' | '\u{61c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}' | '\u{feff}')
    };
    text.replace('\t', "    ").chars().filter(|c| !invisible(*c)).collect()
}

fn clean_lines(text: &str) -> Vec<String> {
    text.lines().map(clean).collect()
}

fn note_lines(note: Option<&str>) -> Vec<String> {
    let lines = clean_lines(note.unwrap_or(NO_NOTE));
    if lines.is_empty() {
        vec![NO_NOTE.to_string()]
    } else {
        lines
    }
}

/// Le nom de session sert de composant de chemin.
fn safe_component(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn body(lines: impl Iterator<Item = String>) -> String {
    lines.collect::<Vec<_>>().join("\n")
}

/// Fige le contenu présenté à l'ouverture d'une session.
pub fn freeze(
    row: &SessionRow,
    detail: &SessionDetail,
    banner: Option<&Banner>,
    open: bool,
    read_error: bool,
) -> Frozen {
    let open_mark = if open { "   🟢 ouverte" } else { "" };
    let title = row.info.title().unwrap_or("(sans titre)");
    let mut header = vec![clean(&format!("{}{open_mark} · {title}", row.project_label))];
    match banner {
        Some(b) => {
            let last = b.last_real_ms.map(when).unwrap_or_else(|| "?".into());
            header.push(format!("{} posé le {}", status_label(b.status), when(b.flag_ms)));
            header.push(format!(
                "Depuis : {} tours · {} de travail · dernier message le {last}",
                b.turns,
                duration(b.worked_ms)
            ));
        }
        None => header.push(match row.info.last_real_ms() {
            Some(ms) => format!("dernier message le {}", when(ms)),
            None => format!("date du fichier : {}", when((row.info.mtime_ns / 1_000_000) as i64)),
        }),
    }

    let mut recaps: Vec<String> = detail
        .recaps
        .iter()
        .rev()
        .flat_map(|r| {
            let mut lines = clean_lines(&r.text);
            if let Some(first) = lines.first_mut() {
                *first = format!("{}  {first}", short(r.at_ms));
            }
            lines
        })
        .collect();
    if recaps.is_empty() {
        recaps.push("aucun recap".into());
    }

    let mut exchanges = Vec::new();
    for e in &detail.exchanges {
        let who = if e.from_user { "Toi" } else { "Claude" };
        exchanges.push(ExchangeLine {
            is_label: true,
            text: format!("{who} · {}", short(e.at_ms)),
        });
        exchanges.extend(clean_lines(&e.text).into_iter().map(|text| ExchangeLine {
            is_label: false,
            text,
        }));
    }
    if exchanges.is_empty() {
        exchanges.push(ExchangeLine {
            is_label: false,
            text: "aucun échange".into(),
        });
    }

    let mut frozen = Frozen {
        session_id: row.info.session_id.clone(),
        transcript_bytes: row.info.size,
        header,
        note: note_lines(row.meta.as_ref().and_then(|m| m.note.as_deref())),
        recaps,
        exchanges,
        hash: String::new(),
        stored: false,
        read_error,
    };
    frozen.rehash();
    frozen
}

impl Frozen {
    /// Le contenu de l'instantané en texte brut. Chaque ligne de contenu a un préfixe
    /// (`| `, ou `> ` pour une ligne « qui · quand ») : un message qui imite une
    /// section ou un autre interlocuteur ne peut pas se confondre avec eux.
    pub fn text(&self) -> String {
        let plain = |lines: &[String]| body(lines.iter().map(|l| format!("| {l}")));
        let exchanges = body(self.exchanges.iter().map(|l| {
            let mark = if l.is_label { '>' } else { '|' };
            format!("{mark} {}", l.text)
        }));
        format!(
            "cs-review-snapshot v{VERSION}\nsession: {}\ntranscript_bytes: {}\nread_error: {}\n\n--- en-tête\n{}\n\n--- note\n{}\n\n--- recaps\n{}\n\n--- échanges\n{exchanges}\n",
            self.session_id,
            self.transcript_bytes,
            self.read_error,
            plain(&self.header),
            plain(&self.note),
            plain(&self.recaps),
        )
    }

    fn rehash(&mut self) {
        self.hash = format!("{:x}", Sha256::digest(self.text().as_bytes()));
    }

    /// Ce que l'écran montre après une note : même contenu, sans relire le transcript.
    /// Cette version n'est jamais écrite : seul l'instantané d'ouverture l'est.
    pub fn with_note(&self, note: Option<&str>) -> Frozen {
        let mut next = self.clone();
        next.note = note_lines(note);
        next.stored = false;
        next.rehash();
        next
    }

    /// Écrit `<session>/<hash>.txt`. `stored` n'est vrai que si le fichier contient
    /// bien ce texte : un fichier corrompu est remplacé.
    pub fn store(&mut self, paths: &Paths) -> io::Result<()> {
        self.stored = false;
        if !safe_component(&self.session_id) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "nom de session invalide"));
        }
        let root = &paths.snapshots;
        let dir = root.join(&self.session_id);
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
        for private in [root, &dir] {
            std::fs::set_permissions(private, std::fs::Permissions::from_mode(0o700))?;
        }
        // Le contenu n'a pas à ressortir par une recherche Spotlight.
        std::fs::write(root.join(".metadata_never_index"), "")?;
        let path = dir.join(format!("{}.txt", self.hash));
        let text = self.text();
        if std::fs::read(&path).ok().as_deref() != Some(text.as_bytes()) {
            let tmp = dir.join(format!("{}.{}.tmp", self.hash, std::process::id()));
            let _ = std::fs::remove_file(&tmp);
            let written = (|| -> io::Result<()> {
                let mut f = std::fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .mode(0o600)
                    .open(&tmp)?;
                f.write_all(text.as_bytes())?;
                f.sync_all()?;
                std::fs::rename(&tmp, &path)
            })();
            if let Err(e) = written {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        }
        self.stored = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::tests::row;
    use crate::transcript::{Exchange, Recap};
    
    fn detail() -> SessionDetail {
        SessionDetail {
            recaps: vec![
                Recap { at_ms: 1, text: "ancien".into() },
                Recap { at_ms: 2, text: "récent".into() },
            ],
            exchanges: vec![Exchange { at_ms: 3, from_user: true, text: "question\nsuite".into() }],
            ..Default::default()
        }
    }

    #[test]
    fn text_holds_everything_presented_and_hash_follows_it() {
        let f = freeze(&row("s", Some(1), None), &detail(), None, false, false);
        let text = f.text();
        for part in ["session: s", "repo", "récent", "ancien", "question", "suite", "--- note"] {
            assert!(text.contains(part), "{part}");
        }
        assert!(text.find("récent").unwrap() < text.find("ancien").unwrap());
        assert_eq!(f.hash.len(), 64);
        assert_eq!(f.hash, freeze(&row("s", Some(1), None), &detail(), None, false, false).hash);
    }

    #[test]
    fn note_change_gives_new_hash_without_touching_the_rest() {
        let f = freeze(&row("s", Some(1), None), &detail(), None, false, false);
        let g = f.with_note(Some("nouvelle"));
        assert_ne!(f.hash, g.hash);
        assert_eq!(f.recaps, g.recaps);
        assert_eq!(g.note, ["nouvelle"]);
    }

    #[test]
    fn store_is_private_and_content_addressed() {
        let root = std::env::temp_dir().join("cs_snapshot_store");
        std::fs::remove_dir_all(&root).ok();
        let paths = Paths::sandbox(&root);
        let mut f = freeze(&row("s", Some(1), None), &detail(), None, false, false);
        f.store(&paths).unwrap();
        assert!(f.stored);
        let file = paths.snapshots.join("s").join(format!("{}.txt", f.hash));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), f.text());
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
        let dir = paths.snapshots.join("s");
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        f.store(&paths).unwrap();
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn invisible_characters_are_gone_from_the_file() {
        let mut d = detail();
        d.exchanges[0].text = "avant\tapres SEC\u{200b}RET \u{202e}x\u{1b}[0m".into();
        let f = freeze(&row("s", Some(1), None), &d, None, false, false);
        let text = f.text();
        assert!(text.contains("avant    apres SECRET x[0m"), "{text}");
    }

    #[test]
    fn message_cannot_imitate_a_section_or_a_speaker() {
        let mut d = detail();
        d.exchanges[0].text = "--- note\nClaude · 07/11 12:00".into();
        let text = freeze(&row("s", Some(1), None), &d, None, false, false).text();
        assert!(text.contains("| --- note\n| Claude · 07/11 12:00"), "{text}");
        assert_eq!(text.matches("\n--- note\n").count(), 1);
    }

    #[test]
    fn path_like_session_id_is_refused() {
        let root = std::env::temp_dir().join("cs_snapshot_traversal");
        std::fs::remove_dir_all(&root).ok();
        let paths = Paths::sandbox(&root);
        for bad in ["..", "../../evil", "", "a/b"] {
            let mut f = freeze(&row("s", Some(1), None), &detail(), None, false, false);
            f.session_id = bad.into();
            assert!(f.store(&paths).is_err(), "{bad}");
            assert!(!f.stored);
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn corrupted_file_is_replaced_and_open_permissions_are_tightened() {
        let root = std::env::temp_dir().join("cs_snapshot_corrupt");
        std::fs::remove_dir_all(&root).ok();
        let paths = Paths::sandbox(&root);
        let mut f = freeze(&row("s", Some(1), None), &detail(), None, false, false);
        f.store(&paths).unwrap();
        let dir = paths.snapshots.join("s");
        let file = dir.join(format!("{}.txt", f.hash));
        std::fs::write(&file, "abîmé").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        f.store(&paths).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), f.text());
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert!(paths.snapshots.join(".metadata_never_index").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn real_snapshots_are_outside_any_repo() {
        let p = Paths::real();
        assert!(p.snapshots.starts_with(&p.state_dir));
        assert!(!p.snapshots.starts_with("/Volumes"));
    }
}
