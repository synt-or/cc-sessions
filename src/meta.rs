use crate::model::{SessionMeta, Status};
use crate::project::git_root;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Chemin du fichier de notes local pour un cwd donné (git-root sinon cwd).
pub fn notes_file(cwd: &str) -> PathBuf {
    let root = git_root(cwd).unwrap_or_else(|| cwd.to_string());
    Path::new(&root).join(".claude").join("session-notes.jsonl")
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

/// Écrit/remplace l'entrée d'une session dans son fichier de notes local,
/// et garantit l'exclusion git via .git/info/exclude.
pub fn upsert(cwd: &str, session_id: &str, updated_at: &str, status: Status, note: Option<String>) -> std::io::Result<SessionMeta> {
    let file = notes_file(cwd);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    ensure_git_excluded(cwd);

    let mut all = load(&file);
    let meta = SessionMeta {
        session_id: session_id.to_string(),
        updated_at: updated_at.to_string(),
        status,
        note,
    };
    all.insert(session_id.to_string(), meta.clone());

    let mut f = std::fs::File::create(&file)?;
    for m in all.values() {
        writeln!(f, "{}", serde_json::to_string(m).unwrap())?;
    }
    Ok(meta)
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
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&exclude) {
            let _ = writeln!(f, "{entry}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_then_load_roundtrips() {
        let dir = std::env::temp_dir().join("cs_meta_test");
        std::fs::create_dir_all(&dir).unwrap();
        let cwd = dir.to_string_lossy().into_owned();
        upsert(&cwd, "sid1", "2026-06-01T10:00", Status::Hold, Some("attend CI".into())).unwrap();
        upsert(&cwd, "sid1", "2026-06-01T11:00", Status::Done, Some("fini".into())).unwrap();
        let all = load(&notes_file(&cwd));
        assert_eq!(all.len(), 1); // last-wins, pas de doublon
        assert_eq!(all["sid1"].status, Status::Done);
        assert_eq!(all["sid1"].note.as_deref(), Some("fini"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
