use crate::model::SessionInfo;
use crate::scan;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Chemin de l'index cache (XDG). Régénérable, jamais source de vérité.
pub fn index_path() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("cc-sessions")
        .join("index.json")
}

/// Charge l'index depuis le disque (clé = chemin du .jsonl).
pub fn load(path: &Path) -> HashMap<String, SessionInfo> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Sauve l'index.
pub fn save(path: &Path, index: &HashMap<String, SessionInfo>) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string(index).unwrap())
}

/// Reconstruit la liste des SessionInfo en réutilisant le cache pour les
/// fichiers dont (mtime, size) sont inchangés ; reparse les autres ; évince
/// les fichiers disparus. `stat` retourne (mtime_ns, size) sans lire le contenu.
pub fn refresh(
    files: &[PathBuf],
    mut cached: HashMap<String, SessionInfo>,
) -> HashMap<String, SessionInfo> {
    let mut out = HashMap::new();
    for f in files {
        let key = f.to_string_lossy().into_owned();
        let meta = match f.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let mtime_ns = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let size = meta.len();
        match cached.remove(&key) {
            Some(prev) if prev.mtime_ns == mtime_ns && prev.size == size => {
                out.insert(key, prev); // hit : aucune lecture du contenu
            }
            _ => {
                if let Ok(info) = scan::scan_file(f) {
                    out.insert(key, info); // miss : reparse
                }
            }
        }
    }
    // tout ce qui reste dans `cached` = fichiers disparus -> évincés (non réinsérés)
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
        let p = write_tmp(&dir, "a.jsonl", "{\"type\":\"ai-title\",\"aiTitle\":\"T\"}\n");
        let first = refresh(&[p.clone()], HashMap::new());
        let key = p.to_string_lossy().into_owned();
        // on injecte une valeur sentinelle dans l'entrée cache : si réutilisée, elle survit
        let mut cached = first.clone();
        cached.get_mut(&key).unwrap().ai_title = Some("SENTINEL".into());
        let second = refresh(&[p.clone()], cached);
        assert_eq!(second.get(&key).unwrap().ai_title.as_deref(), Some("SENTINEL"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn vanished_file_is_evicted() {
        let dir = std::env::temp_dir().join("cs_cache_test_evict");
        std::fs::create_dir_all(&dir).unwrap();
        let p = write_tmp(&dir, "b.jsonl", "{}\n");
        let key = p.to_string_lossy().into_owned();
        let cached = refresh(&[p.clone()], HashMap::new());
        assert!(cached.contains_key(&key));
        std::fs::remove_file(&p).unwrap();
        let after = refresh(&[], cached); // fichier plus listé
        assert!(!after.contains_key(&key));
        std::fs::remove_dir_all(&dir).ok();
    }
}
