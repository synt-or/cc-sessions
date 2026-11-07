use crate::model::{SessionMeta, Status};
use crate::paths::Paths;
use crate::project::git_root;
use chrono::{Local, NaiveDateTime, TimeZone};
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

const NOTES_FILE: &str = "session-notes.jsonl";
const FRAGILE_PREFIXES: [&str; 4] = ["/tmp", "/private/tmp", "/var/folders", "/private/var/folders"];

/// Fichier de notes d'un projet (git-root sinon cwd). `root` : git-root déjà
/// connu dans la boucle chaude du picker.
pub fn project_notes_file(paths: &Paths, cwd: &str, root: Option<&str>) -> PathBuf {
    let base = match &paths.notes_root {
        Some(notes_root) => notes_root.join(cwd.replace('/', "-")),
        None => PathBuf::from(root.unwrap_or(cwd)),
    };
    base.join(".claude").join(NOTES_FILE)
}

/// Magasin central des sessions dont le dossier ne peut pas garder de notes.
pub fn central_notes_file(paths: &Paths) -> PathBuf {
    paths.state_dir.join("orphan-notes.jsonl")
}

/// Le dossier d'origine survivra-t-il pour garder les notes ? Pas s'il a
/// disparu, s'il est temporaire (vidé au redémarrage) ou si c'est un worktree
/// lié (supprimé une fois fusionné).
fn is_fragile(cwd: &str) -> bool {
    let path = Path::new(cwd);
    if !path.is_dir() || FRAGILE_PREFIXES.iter().any(|p| path.starts_with(p)) {
        return true;
    }
    git_root(cwd).is_some_and(|root| Path::new(&root).join(".git").is_file())
}

/// Où écrire les notes d'une session.
pub fn notes_target(paths: &Paths, cwd: &str) -> PathBuf {
    if is_fragile(cwd) {
        central_notes_file(paths)
    } else {
        project_notes_file(paths, cwd, git_root(cwd).as_deref())
    }
}

/// Charge toutes les métadonnées d'un fichier de notes (last-wins par sessionId).
pub fn load(file: &Path) -> HashMap<String, SessionMeta> {
    let mut out = HashMap::new();
    if let Ok(content) = std::fs::read_to_string(file) {
        for line in content.lines() {
            if let Ok(m) = serde_json::from_str::<SessionMeta>(line) {
                out.insert(m.session_id.clone(), m);
            }
        }
    }
    out
}

/// La plus récente de deux entrées : une session peut en avoir une dans chaque
/// magasin si son dossier a disparu puis est revenu.
pub fn most_recent(a: Option<SessionMeta>, b: Option<SessionMeta>) -> Option<SessionMeta> {
    match (a, b) {
        (Some(a), Some(b)) => {
            let newer = (&b.updated_at, &b.decided_at) > (&a.updated_at, &a.decided_at);
            Some(if newer { b } else { a })
        }
        (a, b) => a.or(b),
    }
}

/// État connu d'une session, tous magasins confondus.
pub fn load_for(paths: &Paths, cwd: &str, sid: &str) -> Option<SessionMeta> {
    let project = project_notes_file(paths, cwd, git_root(cwd).as_deref());
    most_recent(
        load(&project).remove(sid),
        load(&central_notes_file(paths)).remove(sid),
    )
}

/// Date de décision d'une entrée, y compris d'une entrée écrite avant `decidedAt`.
///
/// Une telle entrée n'a pour date du flag que son `updatedAt`, que toute écriture
/// remplace : il faut donc la fixer avant de réécrire, sinon la session sortirait
/// de la file sans décision. Fin de minute : `updatedAt` n'a pas de secondes, et
/// un message de la même minute compte comme antérieur au flag.
pub fn effective_decided_at(meta: &SessionMeta) -> Option<String> {
    if meta.decided_at.is_some() {
        return meta.decided_at.clone();
    }
    if meta.status == Status::Active {
        return None;
    }
    let naive = NaiveDateTime::parse_from_str(&meta.updated_at, "%Y-%m-%dT%H:%M").ok()?;
    let local = Local.from_local_datetime(&naive).earliest()?;
    Some((local + chrono::Duration::milliseconds(59_999)).to_rfc3339())
}

/// Sérialise les écritures de notes entre processus `cs` : plusieurs sessions
/// peuvent s'arrêter en même temps, et le magasin central leur est commun. Verrou
/// sous `state_dir`, pour ne pas ajouter de fichier dans les dépôts.
struct NotesLock(std::fs::File);

impl NotesLock {
    fn acquire(paths: &Paths) -> std::io::Result<NotesLock> {
        std::fs::create_dir_all(&paths.state_dir)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(paths.state_dir.join("notes.lock"))?;
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(NotesLock(file))
    }
}

impl Drop for NotesLock {
    fn drop(&mut self) {
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Écrit l'entrée d'une session, sans jamais recréer un dossier d'origine disparu.
fn upsert(paths: &Paths, cwd: &str, meta: SessionMeta) -> std::io::Result<SessionMeta> {
    let _lock = NotesLock::acquire(paths)?;
    let file = notes_target(paths, cwd);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if !paths.is_sandbox() && file != central_notes_file(paths) {
        ensure_git_excluded(cwd);
    }

    let mut all = load(&file);
    all.insert(meta.session_id.clone(), meta.clone());

    // écriture atomique : fichier temporaire dans le même dossier puis rename
    let tmp = file.with_extension(format!("jsonl.{}.tmp", std::process::id()));
    {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)?;
        for m in all.values() {
            writeln!(f, "{}", serde_json::to_string(m).unwrap())?;
        }
        f.sync_all()?;
    }
    std::fs::rename(&tmp, &file)?;
    Ok(meta)
}

/// Pose un statut : c'est une décision de l'utilisateur, datée. Garde la note.
pub fn set_status(paths: &Paths, cwd: &str, sid: &str, status: Status) -> std::io::Result<SessionMeta> {
    let note = load_for(paths, cwd, sid).and_then(|m| m.note);
    upsert(
        paths,
        cwd,
        SessionMeta {
            session_id: sid.to_string(),
            updated_at: crate::now_stamp_pub(),
            status,
            note,
            decided_at: Some(chrono::Local::now().to_rfc3339()),
        },
    )
}

/// Écrit la note. Garde le statut et la date de décision : changer une note ne
/// fait pas croire à un nouveau flag.
pub fn set_note(paths: &Paths, cwd: &str, sid: &str, note: Option<String>) -> std::io::Result<SessionMeta> {
    let prev = load_for(paths, cwd, sid);
    upsert(
        paths,
        cwd,
        SessionMeta {
            session_id: sid.to_string(),
            updated_at: crate::now_stamp_pub(),
            status: prev.as_ref().map(|m| m.status).unwrap_or_default(),
            note,
            decided_at: prev.as_ref().and_then(effective_decided_at),
        },
    )
}

/// Réécrit un état donné, pour l'annulation. N'efface jamais d'entrée.
pub fn restore(
    paths: &Paths,
    cwd: &str,
    sid: &str,
    status: Status,
    note: Option<String>,
    decided_at: Option<String>,
) -> std::io::Result<SessionMeta> {
    upsert(
        paths,
        cwd,
        SessionMeta {
            session_id: sid.to_string(),
            updated_at: crate::now_stamp_pub(),
            status,
            note,
            decided_at,
        },
    )
}

/// Ajoute `.claude/session-notes.jsonl` à .git/info/exclude (idempotent).
fn ensure_git_excluded(cwd: &str) {
    let Some(root) = git_root(cwd) else { return };
    let exclude = Path::new(&root).join(".git").join("info").join("exclude");
    let entry = ".claude/session-notes.jsonl";
    let already = std::fs::read_to_string(&exclude)
        .map(|c| c.lines().any(|l| l.trim() == entry))
        .unwrap_or(false);
    if !already {
        if let Some(dir) = exclude.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&exclude)
        {
            let _ = writeln!(f, "{entry}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bac à sable propre, et un dossier stable hors des préfixes temporaires
    /// (`temp_dir` est sous /var/folders sur macOS) pour les vrais projets.
    fn sandbox(name: &str) -> (Paths, PathBuf) {
        let root = std::env::temp_dir().join(format!("cs_meta_{name}"));
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        (Paths::sandbox(&root), root)
    }

    #[test]
    fn set_status_then_note_roundtrips() {
        let (paths, root) = sandbox("roundtrip");
        set_note(&paths, "/absent", "sid1", Some("attend CI".into())).unwrap();
        set_status(&paths, "/absent", "sid1", Status::Done).unwrap();
        let m = load_for(&paths, "/absent", "sid1").unwrap();
        assert_eq!(m.status, Status::Done);
        assert_eq!(m.note.as_deref(), Some("attend CI"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn editing_note_keeps_decided_at() {
        let (paths, root) = sandbox("keeps");
        let first = set_status(&paths, "/absent", "s", Status::Hold).unwrap();
        assert!(first.decided_at.is_some());
        let second = set_note(&paths, "/absent", "s", Some("n".into())).unwrap();
        assert_eq!(second.decided_at, first.decided_at);
        assert_eq!(second.status, Status::Hold);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn note_on_legacy_entry_keeps_the_flag_date() {
        let (paths, root) = sandbox("legacy");
        std::fs::create_dir_all(&paths.state_dir).unwrap();
        std::fs::write(
            central_notes_file(&paths),
            "{\"sessionId\":\"s\",\"updatedAt\":\"2026-10-12T14:32\",\"status\":\"hold\"}\n",
        )
        .unwrap();
        let written = set_note(&paths, "/absent", "s", Some("n".into())).unwrap();
        let at = written.decided_at.expect("date du flag fixée");
        assert!(at.starts_with("2026-10-12T14:32:59"), "{at}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn active_entry_gets_no_invented_decision() {
        let (paths, root) = sandbox("active");
        let written = set_note(&paths, "/absent", "s", Some("n".into())).unwrap();
        assert_eq!(written.decided_at, None);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn concurrent_writers_lose_nothing() {
        let (paths, root) = sandbox("concurrent");
        let handles: Vec<_> = (0..24)
            .map(|i| {
                let p = paths.clone();
                std::thread::spawn(move || {
                    set_status(&p, "/absent", &format!("s{i}"), Status::Hold).unwrap();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(load(&central_notes_file(&paths)).len(), 24);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn sandbox_notes_never_touch_the_real_cwd() {
        let (paths, root) = sandbox("real_cwd");
        let real = std::env::temp_dir().join("cs_meta_real_repo");
        std::fs::create_dir_all(&real).unwrap();
        let cwd = real.to_string_lossy().into_owned();
        set_status(&paths, &cwd, "s", Status::Hold).unwrap();
        assert!(!real.join(".claude").exists(), "écriture hors du bac à sable");
        assert_eq!(load_for(&paths, &cwd, "s").unwrap().status, Status::Hold);
        assert!(notes_target(&paths, &cwd).starts_with(&root));
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&real).ok();
    }

    #[test]
    fn missing_or_temp_cwd_uses_central_store() {
        let (paths, root) = sandbox("central");
        assert_eq!(notes_target(&paths, "/n/existe/pas"), central_notes_file(&paths));
        assert_eq!(notes_target(&paths, "/private/tmp"), central_notes_file(&paths));
        set_status(&paths, "/n/existe/pas", "s", Status::Hold).unwrap();
        assert!(!Path::new("/n/existe/pas").exists(), "dossier d'origine recréé");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn worktree_uses_central_store() {
        let (paths, root) = sandbox("worktree");
        // Hors /var/folders : le repo de test vit sous le dépôt lui-même.
        let wt = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/cs_meta_wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: /ailleurs\n").unwrap();
        let cwd = wt.to_string_lossy().into_owned();
        assert_eq!(notes_target(&paths, &cwd), central_notes_file(&paths));
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&wt).ok();
    }

    #[test]
    fn stable_project_keeps_its_own_file() {
        let (paths, root) = sandbox("stable");
        let cwd = env!("CARGO_MANIFEST_DIR");
        assert_eq!(
            notes_target(&paths, cwd),
            project_notes_file(&paths, cwd, git_root(cwd).as_deref())
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn most_recent_wins_across_stores() {
        let meta = |at: &str, note: &str| SessionMeta {
            session_id: "s".into(),
            updated_at: at.into(),
            status: Status::Active,
            note: Some(note.into()),
            decided_at: None,
        };
        let got = most_recent(
            Some(meta("2026-06-01T10:00", "vieux")),
            Some(meta("2026-06-02T10:00", "récent")),
        );
        assert_eq!(got.unwrap().note.as_deref(), Some("récent"));
        assert!(most_recent(None, None).is_none());
    }

    #[test]
    fn meta_jsonl_stays_compatible() {
        let line = r#"{"sessionId":"s","updatedAt":"2026-06-01T10:00","status":"hold","note":"n"}"#;
        let m: SessionMeta = serde_json::from_str(line).unwrap();
        assert_eq!(m.decided_at, None);
        let back = serde_json::to_string(&m).unwrap();
        assert!(!back.contains("decidedAt"), "{back}");
    }
}
