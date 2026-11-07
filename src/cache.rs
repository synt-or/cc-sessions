use crate::model::SessionInfo;
use crate::scan;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

const INDEX_VERSION: u32 = 2;

/// Index sur disque. Régénérable, jamais source de vérité.
#[derive(Serialize, Deserialize)]
struct Index {
    version: u32,
    entries: HashMap<String, SessionInfo>,
}

/// Charge l'index (clé = chemin du .jsonl). Illisible ou d'une autre version :
/// index vide, et tout sera relu.
pub fn load(path: &Path) -> HashMap<String, SessionInfo> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Index>(&s).ok())
        .filter(|i| i.version == INDEX_VERSION)
        .map(|i| i.entries)
        .unwrap_or_default()
}

/// Sauve l'index.
pub fn save(path: &Path, index: &HashMap<String, SessionInfo>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let on_disk = serde_json::json!({ "version": INDEX_VERSION, "entries": index });
    // L'index contient des extraits de messages : pas de lecture par les autres comptes.
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    f.write_all(on_disk.to_string().as_bytes())
}

/// Reconstruit la liste des SessionInfo en lisant le moins possible.
///
/// La date d'un transcript bouge sans que son contenu change (session ouverte) :
/// elle ne déclenche aucune lecture. Même taille et même fin : réutilisation.
/// Fichier agrandi dont l'ancienne fin est intacte : lecture depuis l'ancien
/// offset. Sinon : relecture complète. Les fichiers disparus sont évincés.
pub fn refresh(
    files: &[PathBuf],
    mut cached: HashMap<String, SessionInfo>,
) -> HashMap<String, SessionInfo> {
    let mut out = HashMap::new();
    for f in files {
        let key = f.to_string_lossy().into_owned();
        let Ok(meta) = f.metadata() else { continue };
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let size = meta.len();
        let prev = cached
            .remove(&key)
            .filter(|p| p.size <= size)
            .filter(|p| scan::tail_hash(f, p.size).ok() == Some(p.tail_hash));
        let info = match prev {
            Some(mut p) if p.size == size => {
                p.mtime_ns = mtime_ns;
                Some(p)
            }
            Some(p) => scan::scan_file(f, Some(&p)).ok(),
            None => scan::scan_file(f, None).ok(),
        };
        if let Some(info) = info {
            out.insert(key, info);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(dir: &Path, name: &str, body: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn unchanged_file_is_reused_not_reparsed() {
        let dir = std::env::temp_dir().join("cs_cache_test_reuse");
        std::fs::create_dir_all(&dir).unwrap();
        let p = write_tmp(
            &dir,
            "a.jsonl",
            "{\"type\":\"ai-title\",\"aiTitle\":\"T\"}\n",
        );
        let first = refresh(std::slice::from_ref(&p), HashMap::new());
        let key = p.to_string_lossy().into_owned();
        // on injecte une valeur sentinelle dans l'entrée cache : si réutilisée, elle survit
        let mut cached = first.clone();
        cached.get_mut(&key).unwrap().ai_title = Some("SENTINEL".into());
        let second = refresh(std::slice::from_ref(&p), cached);
        assert_eq!(
            second.get(&key).unwrap().ai_title.as_deref(),
            Some("SENTINEL")
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn touched_file_is_reused_without_reading() {
        let dir = std::env::temp_dir().join("cs_cache_test_touch");
        std::fs::create_dir_all(&dir).unwrap();
        let p = write_tmp(&dir, "t.jsonl", "{\"type\":\"ai-title\",\"aiTitle\":\"T\"}\n");
        let key = p.to_string_lossy().into_owned();
        let mut cached = refresh(std::slice::from_ref(&p), HashMap::new());
        let entry = cached.get_mut(&key).unwrap();
        entry.ai_title = Some("SENTINEL".into());
        entry.mtime_ns = 1;
        let after = refresh(std::slice::from_ref(&p), cached);
        assert_eq!(after[&key].ai_title.as_deref(), Some("SENTINEL"));
        assert_ne!(after[&key].mtime_ns, 1, "la date doit être mise à jour");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn grown_file_is_read_incrementally() {
        let dir = std::env::temp_dir().join("cs_cache_test_grow");
        std::fs::create_dir_all(&dir).unwrap();
        let line = "{\"type\":\"user\",\"cwd\":\"/a\",\"timestamp\":\"2026-09-11T10:00:00.000Z\",\"message\":{\"content\":\"un\"}}\n";
        let p = write_tmp(&dir, "g.jsonl", line);
        let key = p.to_string_lossy().into_owned();
        let mut cached = refresh(std::slice::from_ref(&p), HashMap::new());
        assert_eq!(cached[&key].digest.real_count, 1);
        cached.get_mut(&key).unwrap().cwd = Some("SENTINEL".into());
        let offset = cached[&key].digest.offset;
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(line.as_bytes()).unwrap();
        drop(f);
        let after = refresh(std::slice::from_ref(&p), cached);
        assert_eq!(after[&key].digest.real_count, 2);
        assert!(after[&key].digest.offset > offset);
        assert_eq!(after[&key].cwd.as_deref(), Some("SENTINEL"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rewritten_file_is_rescanned() {
        let dir = std::env::temp_dir().join("cs_cache_test_rewrite");
        std::fs::create_dir_all(&dir).unwrap();
        let p = write_tmp(&dir, "r.jsonl", "{\"type\":\"ai-title\",\"aiTitle\":\"AA\"}\n");
        let key = p.to_string_lossy().into_owned();
        let mut cached = refresh(std::slice::from_ref(&p), HashMap::new());
        cached.get_mut(&key).unwrap().ai_title = Some("SENTINEL".into());
        write_tmp(&dir, "r.jsonl", "{\"type\":\"ai-title\",\"aiTitle\":\"BB\"}\n");
        let after = refresh(std::slice::from_ref(&p), cached);
        assert_eq!(after[&key].ai_title.as_deref(), Some("BB"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn old_index_format_is_discarded() {
        let dir = std::env::temp_dir().join("cs_cache_test_old");
        std::fs::create_dir_all(&dir).unwrap();
        let p = write_tmp(&dir, "index.json", "{\"/a.jsonl\":{\"path\":\"/a.jsonl\"}}");
        assert!(load(&p).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn index_roundtrips() {
        let dir = std::env::temp_dir().join("cs_cache_test_roundtrip");
        std::fs::create_dir_all(&dir).unwrap();
        let p = write_tmp(&dir, "s.jsonl", "{\"type\":\"ai-title\",\"aiTitle\":\"T\"}\n");
        let index = refresh(std::slice::from_ref(&p), HashMap::new());
        let idx = dir.join("index.json");
        save(&idx, &index).unwrap();
        assert_eq!(load(&idx), index);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn vanished_file_is_evicted() {
        let dir = std::env::temp_dir().join("cs_cache_test_evict");
        std::fs::create_dir_all(&dir).unwrap();
        let p = write_tmp(&dir, "b.jsonl", "{}\n");
        let key = p.to_string_lossy().into_owned();
        let cached = refresh(std::slice::from_ref(&p), HashMap::new());
        assert!(cached.contains_key(&key));
        std::fs::remove_file(&p).unwrap();
        let after = refresh(&[], cached); // fichier plus listé
        assert!(!after.contains_key(&key));
        std::fs::remove_dir_all(&dir).ok();
    }
}
