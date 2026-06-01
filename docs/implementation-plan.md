# cc-sessions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Construire `cs`, un CLI Rust qui liste/reprend les sessions Claude Code cross-projet, avec notes + statuts locaux par projet, stats et archivage.

**Architecture:** Binaire unique en modules isolés : `scan` (extraction head/tail des `.jsonl`), `cache` (index `(mtime,size)`), `meta` (notes/statuts JSONL locaux), `project` (libellé repo/sous-dossier), `picker` (skim), `archive`, `stats`, `cli` (dispatch clap). Stockage local par projet, cache XDG régénérable. Packaging via `flake.nix` (`rustPlatform.buildRustPackage`), consommé en input par le flake `infra`.

**Tech Stack:** Rust 1.95, clap (derive), serde + serde_json, skim, anyhow, dirs. Nix flake pour le packaging.

**Emplacement du repo :** `~/Documents/temp/cc-sessions` (repo git dédié). Le plan et la spec vivent dans `infra/doc/superpowers/`.

**Référence spec :** `doc/superpowers/specs/2026-06-01-cc-sessions-cli-design.md`

---

## File Structure

```
cc-sessions/
├── Cargo.toml          # deps + métadonnées crate
├── Cargo.lock          # épinglage (commité)
├── flake.nix           # buildRustPackage, expose packages.default
├── .gitignore          # /target
└── src/
    ├── main.rs         # entrée : parse CLI, dispatch
    ├── cli.rs          # définition clap (Args, Command)
    ├── model.rs        # Status, SessionMeta, SessionInfo, SessionRow
    ├── project.rs      # libellé repo/sous-dossier depuis un cwd
    ├── scan.rs         # extraction head/tail d'un .jsonl -> SessionInfo
    ├── cache.rs        # index ~/.cache/cc-sessions/index.json
    ├── meta.rs         # session-notes.jsonl local + git-root + exclude
    ├── stats.rs        # agrégation par projet
    ├── archive.rs      # déplacement non destructif + purge
    └── picker.rs       # rendu skim + sélection
```

Chaque module a une responsabilité unique et des tests unitaires (sauf `picker`, testé à la main car interactif).

---

## Task 0: Scaffold du repo et build minimal

**Files:**
- Create: `~/Documents/temp/cc-sessions/Cargo.toml`
- Create: `~/Documents/temp/cc-sessions/src/main.rs`
- Create: `~/Documents/temp/cc-sessions/.gitignore`

- [ ] **Step 1: Initialiser le repo**

```bash
mkdir -p ~/Documents/temp/cc-sessions && cd ~/Documents/temp/cc-sessions
git init
printf '/target\n' > .gitignore
```

- [ ] **Step 2: Écrire `Cargo.toml`**

```toml
[package]
name = "cc-sessions"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "cs"
path = "src/main.rs"

[dependencies]
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
anyhow = "1"
dirs = "5"
skim = "0.10"

[profile.release]
strip = true
```

- [ ] **Step 3: `src/main.rs` minimal**

```rust
fn main() {
    println!("cs: ok");
}
```

- [ ] **Step 4: Build et run**

Run: `cd ~/Documents/temp/cc-sessions && cargo run`
Expected: compile, affiche `cs: ok`

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "chore: scaffold cc-sessions crate"
```

---

## Task 1: Modèle de données (`model.rs`)

**Files:**
- Create: `src/model.rs`
- Modify: `src/main.rs` (déclarer `mod model;`)

- [ ] **Step 1: Écrire le test d'abord — `src/model.rs`**

```rust
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
```

- [ ] **Step 2: Déclarer le module dans `src/main.rs`**

```rust
mod model;

fn main() {
    println!("cs: ok");
}
```

- [ ] **Step 3: Lancer les tests**

Run: `cargo test model`
Expected: 3 tests PASS

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(model): Status, SessionMeta, SessionInfo + serde"
```

---

## Task 2: Extraction d'un `.jsonl` (`scan.rs`)

**Files:**
- Create: `src/scan.rs`
- Create: `tests/fixtures/sample.jsonl`
- Modify: `src/main.rs` (`mod scan;`)

- [ ] **Step 1: Créer une fixture — `tests/fixtures/sample.jsonl`**

```
{"type":"user","cwd":"/Users/x/proj","message":{"content":"première vraie question"}}
{"type":"assistant","cwd":"/Users/x/proj","message":{"content":"réponse"}}
{"type":"ai-title","aiTitle":"Mon titre auto","sessionId":"sample"}
{"type":"last-prompt","lastPrompt":"le dernier prompt utilisateur","sessionId":"sample"}
```

- [ ] **Step 2: Écrire `src/scan.rs` avec ses tests**

```rust
use crate::model::SessionInfo;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const HEAD: u64 = 32 * 1024;
const TAIL: u64 = 64 * 1024;

/// Lit les `n` premiers octets d'un fichier.
fn read_head(path: &Path, n: u64) -> std::io::Result<String> {
    let mut f = File::open(path)?;
    let mut buf = vec![0u8; n as usize];
    let read = f.read(&mut buf)?;
    buf.truncate(read);
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Lit les `n` derniers octets d'un fichier.
fn read_tail(path: &Path, n: u64) -> std::io::Result<String> {
    let mut f = File::open(path)?;
    let size = f.metadata()?.len();
    let start = size.saturating_sub(n);
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Extrait les champs utiles d'un texte JSONL (concat head+tail).
fn extract_fields(text: &str) -> (Option<String>, Option<String>, Option<String>, Option<String>) {
    let (mut cwd, mut ai_title, mut last_prompt, mut first_user) = (None, None, None, None);
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('{') {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(c) = v.get("cwd").and_then(|x| x.as_str()) {
            cwd = Some(c.to_string());
        }
        match v.get("type").and_then(|x| x.as_str()) {
            Some("ai-title") => {
                if let Some(t) = v.get("aiTitle").and_then(|x| x.as_str()) {
                    ai_title = Some(t.to_string());
                }
            }
            Some("last-prompt") => {
                if let Some(p) = v.get("lastPrompt").and_then(|x| x.as_str()) {
                    last_prompt = Some(p.to_string());
                }
            }
            Some("user") if first_user.is_none() => {
                let content = v.get("message").and_then(|m| m.get("content"));
                let text = match content {
                    Some(serde_json::Value::String(s)) => Some(s.clone()),
                    Some(serde_json::Value::Array(arr)) => arr
                        .iter()
                        .find_map(|p| {
                            if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                                p.get("text").and_then(|t| t.as_str()).map(String::from)
                            } else {
                                None
                            }
                        }),
                    _ => None,
                };
                // ignore les caveats / bang-commands
                if let Some(t) = text {
                    if !t.trim_start().starts_with('<') {
                        first_user = Some(t);
                    }
                }
            }
            _ => {}
        }
    }
    (cwd, ai_title, last_prompt, first_user)
}

/// Construit un SessionInfo pour un fichier `.jsonl`.
pub fn scan_file(path: &Path) -> std::io::Result<SessionInfo> {
    let meta = path.metadata()?;
    let mtime_ns = meta
        .modified()?
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let text = format!("{}\n{}", read_head(path, HEAD)?, read_tail(path, TAIL)?);
    let (cwd, ai_title, last_prompt, first_user) = extract_fields(&text);
    let session_id = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    Ok(SessionInfo {
        path: PathBuf::from(path),
        session_id,
        mtime_ns,
        size: meta.len(),
        cwd,
        ai_title,
        last_prompt,
        first_user,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.jsonl")
    }

    #[test]
    fn extracts_title_and_prompt_and_cwd() {
        let info = scan_file(&fixture()).unwrap();
        assert_eq!(info.ai_title.as_deref(), Some("Mon titre auto"));
        assert_eq!(info.last_prompt.as_deref(), Some("le dernier prompt utilisateur"));
        assert_eq!(info.cwd.as_deref(), Some("/Users/x/proj"));
        assert_eq!(info.first_user.as_deref(), Some("première vraie question"));
        assert_eq!(info.session_id, "sample");
        assert!(info.size > 0);
    }

    #[test]
    fn ignores_caveat_first_user() {
        let text = r#"{"type":"user","message":{"content":"<local-command-caveat>blah"}}"#;
        let (_, _, _, first_user) = extract_fields(text);
        assert_eq!(first_user, None);
    }
}
```

- [ ] **Step 3: Déclarer `mod scan;` dans `main.rs`** (ajouter la ligne sous `mod model;`)

- [ ] **Step 4: Tests**

Run: `cargo test scan`
Expected: 2 tests PASS

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat(scan): extraction head/tail des .jsonl"
```

---

## Task 3: Libellé projet (`project.rs`)

**Files:**
- Create: `src/project.rs`
- Modify: `src/main.rs` (`mod project;`)

- [ ] **Step 1: Écrire `src/project.rs` avec tests**

```rust
use std::path::Path;
use std::process::Command;

/// Renvoie le git-root d'un répertoire, ou None si hors d'un repo.
pub fn git_root(cwd: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", cwd, "rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if root.is_empty() {
        None
    } else {
        Some(root)
    }
}

/// Libellé d'affichage : "repo" ou "repo/sous-dossier" si git-root connu,
/// sinon les deux derniers segments du chemin.
pub fn label(cwd: &str, root: Option<&str>) -> String {
    match root {
        Some(root) => {
            let repo = Path::new(root)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.to_string());
            let rel = cwd.strip_prefix(root).unwrap_or("").trim_start_matches('/');
            if rel.is_empty() {
                repo
            } else {
                format!("{repo}/{rel}")
            }
        }
        None => {
            let p = Path::new(cwd);
            let last = p.file_name().map(|s| s.to_string_lossy().into_owned());
            let parent = p
                .parent()
                .and_then(|x| x.file_name())
                .map(|s| s.to_string_lossy().into_owned());
            match (parent, last) {
                (Some(par), Some(l)) => format!("{par}/{l}"),
                (None, Some(l)) => l,
                _ => cwd.to_string(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_root_gives_repo_name() {
        assert_eq!(label("/Users/x/temp/infra", Some("/Users/x/temp/infra")), "infra");
    }

    #[test]
    fn subdir_gives_repo_slash_subdir() {
        assert_eq!(
            label("/Users/x/temp/A2A-COMM/cli", Some("/Users/x/temp/A2A-COMM")),
            "A2A-COMM/cli"
        );
    }

    #[test]
    fn non_git_gives_last_two_segments() {
        assert_eq!(label("/Users/x/email-triage-log/labels", None), "email-triage-log/labels");
    }
}
```

- [ ] **Step 2: `mod project;` dans `main.rs`**

- [ ] **Step 3: Tests**

Run: `cargo test project`
Expected: 3 tests PASS

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(project): libellé repo/sous-dossier"
```

---

## Task 4: Cache d'index (`cache.rs`)

**Files:**
- Create: `src/cache.rs`
- Modify: `src/main.rs` (`mod cache;`)

- [ ] **Step 1: Écrire `src/cache.rs` avec tests**

```rust
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
```

- [ ] **Step 2: `mod cache;` dans `main.rs`**

- [ ] **Step 3: Tests**

Run: `cargo test cache`
Expected: 2 tests PASS

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(cache): index (mtime,size) régénérable"
```

---

## Task 5: Notes/statuts locaux (`meta.rs`)

**Files:**
- Create: `src/meta.rs`
- Modify: `src/main.rs` (`mod meta;`)

- [ ] **Step 1: Écrire `src/meta.rs` avec tests**

```rust
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
```

Note : le test tourne dans un dossier temp non-git → `git_root` renvoie None, `ensure_git_excluded` est un no-op, et `notes_file` retombe sur le cwd. Comportement attendu.

- [ ] **Step 2: `mod meta;` dans `main.rs`**

- [ ] **Step 3: Tests**

Run: `cargo test meta`
Expected: 1 test PASS

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(meta): notes/statuts JSONL locaux + git exclude"
```

---

## Task 6: Découverte des sessions + assemblage des lignes (`model.rs` étendu)

**Files:**
- Modify: `src/model.rs` (ajouter `SessionRow` + helpers d'affichage)
- Create: dossier de découverte dans `src/scan.rs` (fonction `discover`)

- [ ] **Step 1: Ajouter `discover` à `src/scan.rs`**

```rust
/// Liste les .jsonl de toutes les sessions, hors `subagents` et fichiers < 2 Kio.
pub fn discover(projects_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(entries) = std::fs::read_dir(projects_dir) else { return files };
    for proj in entries.flatten() {
        if !proj.path().is_dir() {
            continue;
        }
        if proj.file_name().to_string_lossy() == "subagents" {
            continue;
        }
        if let Ok(sessions) = std::fs::read_dir(proj.path()) {
            for s in sessions.flatten() {
                let p = s.path();
                if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                    if p.metadata().map(|m| m.len() >= 2048).unwrap_or(false) {
                        files.push(p);
                    }
                }
            }
        }
    }
    files
}
```

- [ ] **Step 2: Ajouter `SessionRow` + résumé/tri à `src/model.rs`**

```rust
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
```

- [ ] **Step 3: Tests**

Run: `cargo test`
Expected: tous PASS (model + scan + project + cache + meta + row)

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(model): SessionRow (résumé, tri par statut) + scan::discover"
```

---

## Task 7: Assemblage central — `rows()` (`main.rs` ou `lib`)

**Files:**
- Create: `src/build.rs` (assemblage scan+cache+meta+project → `Vec<SessionRow>`)
- Modify: `src/main.rs` (`mod build;`)

- [ ] **Step 1: Écrire `src/build.rs`**

```rust
use crate::model::SessionRow;
use crate::{cache, meta, project, scan};
use std::collections::HashMap;
use std::path::PathBuf;

const MAX_ROWS: usize = 300;

/// Pipeline complet : découverte -> cache -> méta locale -> tri.
pub fn rows(projects_dir: &PathBuf) -> Vec<SessionRow> {
    let files = scan::discover(projects_dir);
    let idx_path = cache::index_path();
    let cached = cache::load(&idx_path);
    let index = cache::refresh(&files, cached);
    let _ = cache::save(&idx_path, &index);

    // cache des notes par git-root pour éviter de relire le même fichier
    let mut notes_cache: HashMap<String, HashMap<String, crate::model::SessionMeta>> = HashMap::new();

    let mut rows: Vec<SessionRow> = index
        .into_values()
        .map(|info| {
            let cwd = info.cwd.clone().unwrap_or_default();
            let root = project::git_root(&cwd);
            let label = project::label(&cwd, root.as_deref());
            let notes_file = meta::notes_file(&cwd);
            let key = notes_file.to_string_lossy().into_owned();
            let metas = notes_cache
                .entry(key)
                .or_insert_with(|| meta::load(&notes_file));
            let m = metas.get(&info.session_id).cloned();
            SessionRow { info, meta: m, project_label: label }
        })
        .collect();

    rows.sort_by_key(|r| r.sort_key());
    rows.truncate(MAX_ROWS);
    rows
}
```

- [ ] **Step 2: `mod build;` dans `main.rs`**

- [ ] **Step 3: Vérifier la compilation**

Run: `cargo build`
Expected: compile sans erreur (warnings unused OK à ce stade)

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(build): pipeline scan+cache+meta+tri -> SessionRow"
```

---

## Task 8: CLI clap + sous-commandes note/statut (`cli.rs`)

**Files:**
- Create: `src/cli.rs`
- Rewrite: `src/main.rs` (dispatch réel)

- [ ] **Step 1: Écrire `src/cli.rs`**

```rust
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "cs", about = "Picker & gestion des sessions Claude Code")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Écrire/remplacer la note de la session courante
    Note {
        text: Vec<String>,
        /// Ajouter à la note existante
        #[arg(short = 'a', long)]
        append: bool,
    },
    /// Marquer la session courante en attente
    Hold,
    /// Marquer la session courante terminée
    Done {
        /// Marquer en masse les sessions inactives depuis N jours (ex: 30d)
        #[arg(long)]
        older_than: Option<String>,
    },
    /// Réactiver la session courante
    Active,
    /// Statistiques par projet
    Stats,
    /// Archiver des sessions (non destructif)
    Archive {
        #[arg(long)]
        older_than: Option<String>,
        uuids: Vec<String>,
    },
    /// Vider définitivement l'archive (double confirmation)
    PurgeArchive,
}

/// Lit l'identifiant de la session Claude Code courante.
pub fn current_session_id() -> anyhow::Result<String> {
    std::env::var("CLAUDE_CODE_SESSION_ID")
        .map_err(|_| anyhow::anyhow!("CLAUDE_CODE_SESSION_ID absent — lance ceci depuis l'intérieur d'une session (« ! cs note … »)"))
}
```

- [ ] **Step 2: Réécrire `src/main.rs`**

```rust
mod build;
mod cache;
mod cli;
mod meta;
mod model;
mod picker;
mod project;
mod scan;
mod stats;
mod archive;

use anyhow::Result;
use clap::Parser;
use model::Status;
use std::path::PathBuf;

fn projects_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".claude").join("projects")
}

/// Horodatage léger sans dépendance chrono : `date '+%Y-%m-%dT%H:%M'`.
fn now_stamp() -> String {
    std::process::Command::new("date")
        .args(["+%Y-%m-%dT%H:%M"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// Trouve le cwd d'une session via l'index (pour rattacher note/statut au bon projet).
fn cwd_of_session(sid: &str) -> Option<String> {
    let idx = cache::load(&cache::index_path());
    idx.values().find(|i| i.session_id == sid).and_then(|i| i.cwd.clone())
}

fn set_status(status: Status) -> Result<()> {
    let sid = cli::current_session_id()?;
    let cwd = cwd_of_session(&sid).unwrap_or_else(|| ".".to_string());
    let existing = meta::load(&meta::notes_file(&cwd)).get(&sid).and_then(|m| m.note.clone());
    meta::upsert(&cwd, &sid, &now_stamp(), status, existing)?;
    println!("✓ statut={status:?} pour {sid}");
    Ok(())
}

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    match cli.command {
        None => picker::run(&build::rows(&projects_dir())),
        Some(cli::Command::Note { text, append }) => {
            let sid = cli::current_session_id()?;
            let cwd = cwd_of_session(&sid).unwrap_or_else(|| ".".to_string());
            let mut note = text.join(" ");
            if append {
                if let Some(prev) = meta::load(&meta::notes_file(&cwd)).get(&sid).and_then(|m| m.note.clone()) {
                    note = format!("{prev} ⏎ {note}");
                }
            }
            let status = meta::load(&meta::notes_file(&cwd)).get(&sid).map(|m| m.status).unwrap_or_default();
            let m = meta::upsert(&cwd, &sid, &now_stamp(), status, Some(note))?;
            println!("✓ note ({sid})\n  {}", m.note.unwrap_or_default());
            Ok(())
        }
        Some(cli::Command::Hold) => set_status(Status::Hold),
        Some(cli::Command::Active) => set_status(Status::Active),
        Some(cli::Command::Done { older_than }) => {
            match older_than {
                Some(spec) => stats::mark_done_older_than(&projects_dir(), &spec),
                None => set_status(Status::Done),
            }
        }
        Some(cli::Command::Stats) => stats::print_stats(&projects_dir()),
        Some(cli::Command::Archive { older_than, uuids }) => archive::archive(&projects_dir(), older_than.as_deref(), &uuids),
        Some(cli::Command::PurgeArchive) => archive::purge_archive(),
    }
}
```

- [ ] **Step 3: Compiler (échouera : `stats`, `archive`, `picker` pas encore écrits)**

Run: `cargo build`
Expected: erreurs « unresolved module » pour stats/archive/picker → normal, on les écrit aux tâches suivantes. NE PAS commiter encore.

---

## Task 9: Stats + `done --older-than` (`stats.rs`)

**Files:**
- Create: `src/stats.rs`

- [ ] **Step 1: Écrire `src/stats.rs` avec test de parsing d'âge**

```rust
use crate::model::Status;
use crate::{build, meta};
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Parse "30d" / "12h" -> nanosecondes. None si invalide.
pub fn parse_age_ns(spec: &str) -> Option<u64> {
    let spec = spec.trim();
    let (num, unit) = spec.split_at(spec.len().saturating_sub(1));
    let n: u64 = num.parse().ok()?;
    let mult_secs = match unit {
        "d" => 86_400,
        "h" => 3_600,
        _ => return None,
    };
    Some(n * mult_secs * 1_000_000_000)
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Affiche un récap par projet.
pub fn print_stats(projects_dir: &PathBuf) -> Result<()> {
    let rows = build::rows(projects_dir);
    let mut by_proj: BTreeMap<String, (usize, u64, u64)> = BTreeMap::new(); // (nb, taille, mtime_max)
    for r in &rows {
        let e = by_proj.entry(r.project_label.clone()).or_insert((0, 0, 0));
        e.0 += 1;
        e.1 += r.info.size;
        e.2 = e.2.max(r.info.mtime_ns);
    }
    println!("{:<30} {:>6} {:>10}", "projet", "sess.", "taille");
    for (proj, (n, size, _)) in &by_proj {
        println!("{:<30} {:>6} {:>9}M", proj, n, size / 1_048_576);
    }
    println!("\n{} sessions affichées (max 300).", rows.len());
    Ok(())
}

/// Marque `done` toutes les sessions inactives depuis l'âge donné.
pub fn mark_done_older_than(projects_dir: &PathBuf, spec: &str) -> Result<()> {
    let age = parse_age_ns(spec).ok_or_else(|| anyhow::anyhow!("âge invalide: {spec} (ex: 30d, 12h)"))?;
    let cutoff = now_ns().saturating_sub(age);
    let rows = build::rows(projects_dir);
    let mut n = 0;
    let stamp = crate::now_stamp_pub();
    for r in rows.iter().filter(|r| r.info.mtime_ns < cutoff && r.status() != Status::Done) {
        if let Some(cwd) = &r.info.cwd {
            let note = r.meta.as_ref().and_then(|m| m.note.clone());
            meta::upsert(cwd, &r.info.session_id, &stamp, Status::Done, note)?;
            n += 1;
        }
    }
    println!("✓ {n} sessions marquées done (inactives depuis {spec}).");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_days_and_hours() {
        assert_eq!(parse_age_ns("1d"), Some(86_400 * 1_000_000_000));
        assert_eq!(parse_age_ns("2h"), Some(7_200 * 1_000_000_000));
        assert_eq!(parse_age_ns("xx"), None);
    }
}
```

- [ ] **Step 2: Exposer `now_stamp` depuis `main.rs`**

Ajouter dans `src/main.rs` :

```rust
pub fn now_stamp_pub() -> String {
    now_stamp()
}
```

- [ ] **Step 3: Tests**

Run: `cargo test stats`
Expected: 1 test PASS (la compilation des autres modules peut encore échouer si `archive`/`picker` manquent ; sinon `cargo test stats::` cible le module)

- [ ] **Step 4: Commit (après Task 11 si la compilation globale bloque)**

```bash
git add -A && git commit -m "feat(stats): récap par projet + done --older-than"
```

---

## Task 10: Archivage + purge (`archive.rs`)

**Files:**
- Create: `src/archive.rs`

- [ ] **Step 1: Écrire `src/archive.rs`**

```rust
use crate::stats::parse_age_ns;
use crate::{build, scan};
use anyhow::Result;
use std::io::{self, Write};
use std::path::PathBuf;

fn archive_root() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".claude").join("projects-archive")
}

fn projects_root() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".claude").join("projects")
}

/// Déplace les .jsonl ciblés (par âge et/ou uuids) vers l'archive, en conservant
/// l'arborescence relative à ~/.claude/projects.
pub fn archive(projects_dir: &PathBuf, older_than: Option<&str>, uuids: &[String]) -> Result<()> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos() as u64;
    let cutoff = older_than.and_then(parse_age_ns).map(|a| now.saturating_sub(a));
    let files = scan::discover(projects_dir);
    let mut moved = 0;
    for f in files {
        let sid = f.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        let by_uuid = uuids.iter().any(|u| u == &sid);
        let by_age = cutoff
            .map(|c| f.metadata().ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| (d.as_nanos() as u64) < c).unwrap_or(false))
            .unwrap_or(false);
        if !(by_uuid || by_age) {
            continue;
        }
        let rel = f.strip_prefix(projects_root()).unwrap_or(&f);
        let dest = archive_root().join(rel);
        if let Some(d) = dest.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::rename(&f, &dest)?;
        moved += 1;
    }
    // l'index sera reconstruit au prochain lancement (fichiers disparus évincés)
    let _ = build::rows(projects_dir);
    println!("✓ {moved} session(s) archivée(s) dans {}", archive_root().display());
    Ok(())
}

/// Supprime définitivement l'archive après double confirmation.
pub fn purge_archive() -> Result<()> {
    let root = archive_root();
    if !root.exists() {
        println!("Archive vide, rien à purger.");
        return Ok(());
    }
    let count = walk_count(&root);
    println!("⚠️  Suppression DÉFINITIVE de {count} fichier(s) dans {}", root.display());
    print!("Taper « SUPPRIMER » pour confirmer : ");
    io::stdout().flush()?;
    let mut a = String::new();
    io::stdin().read_line(&mut a)?;
    if a.trim() != "SUPPRIMER" {
        println!("Annulé.");
        return Ok(());
    }
    print!("Confirmer une dernière fois (o/N) : ");
    io::stdout().flush()?;
    let mut b = String::new();
    io::stdin().read_line(&mut b)?;
    if b.trim().eq_ignore_ascii_case("o") {
        std::fs::remove_dir_all(&root)?;
        println!("✓ Archive purgée.");
    } else {
        println!("Annulé.");
    }
    Ok(())
}

fn walk_count(dir: &PathBuf) -> usize {
    let mut n = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                n += walk_count(&p);
            } else {
                n += 1;
            }
        }
    }
    n
}
```

- [ ] **Step 2: Compiler**

Run: `cargo build`
Expected: il reste `picker` à écrire (Task 11) → erreur sur `mod picker`. Continuer.

---

## Task 11: Picker skim + reprise (`picker.rs`)

**Files:**
- Create: `src/picker.rs`

> **Note d'implémentation :** l'API exacte de `skim` dépend de la version épinglée
> (`skim = "0.10"` dans Cargo.toml). Vérifier `cargo doc -p skim --open` et ajuster
> `SkimItem`/`SkimOptionsBuilder` si l'API diffère. Le code ci-dessous cible 0.10.

- [ ] **Step 1: Écrire `src/picker.rs`**

```rust
use crate::model::{SessionRow, Status};
use anyhow::Result;
use skim::prelude::*;
use std::borrow::Cow;
use std::os::unix::process::CommandExt;
use std::sync::Arc;

struct Item {
    display: String,
    sid: String,
    cwd: String,
    preview: String,
}

impl SkimItem for Item {
    fn text(&self) -> Cow<str> {
        Cow::Borrowed(&self.display)
    }
    fn preview(&self, _ctx: PreviewContext) -> ItemPreview {
        ItemPreview::Text(self.preview.clone())
    }
    fn output(&self) -> Cow<str> {
        Cow::Owned(format!("{}\t{}", self.sid, self.cwd))
    }
}

fn icon(status: Status, has_note: bool) -> &'static str {
    match (status, has_note) {
        (Status::Hold, _) => "⏳",
        (Status::Done, _) => "✓ ",
        (_, true) => "📝",
        _ => "  ",
    }
}

/// Lance le picker ; à la sélection, cd + execvp claude --resume.
pub fn run(rows: &[SessionRow]) -> Result<()> {
    let show_done = false; // toggle futur via re-lancement ; done masquées par défaut
    let options = SkimOptionsBuilder::default()
        .height(Some("90%"))
        .reverse(true)
        .prompt(Some("claude session ❯ "))
        .preview(Some("")) // preview fournie par l'item
        .preview_window(Some("right:55%:wrap"))
        .build()
        .unwrap();

    let (tx, rx): (SkimItemSender, SkimItemReceiver) = unbounded();
    for r in rows {
        if !show_done && r.status() == Status::Done {
            continue;
        }
        let date = ""; // (format mtime ci-dessous)
        let _ = date;
        let display = format!(
            "{}\t{}\t{}\t{}",
            icon(r.status(), r.meta.as_ref().and_then(|m| m.note.as_ref()).is_some()),
            r.project_label,
            r.summary(),
            r.info.session_id
        );
        let preview = format!(
            "{}\nprojet : {}\nuuid : {}\nstatut : {:?}\n\nnote : {}\n\ndernier prompt :\n{}",
            r.info.ai_title.clone().unwrap_or_else(|| "(sans titre)".into()),
            r.info.cwd.clone().unwrap_or_default(),
            r.info.session_id,
            r.status(),
            r.meta.as_ref().and_then(|m| m.note.clone()).unwrap_or_else(|| "—".into()),
            r.info.last_prompt.clone().unwrap_or_default(),
        );
        let _ = tx.send(Arc::new(Item {
            display,
            sid: r.info.session_id.clone(),
            cwd: r.info.cwd.clone().unwrap_or_default(),
            preview,
        }));
    }
    drop(tx);

    let selected = Skim::run_with(&options, Some(rx))
        .filter(|o| !o.is_abort)
        .map(|o| o.selected_items)
        .unwrap_or_default();

    if let Some(item) = selected.first() {
        let out = item.output();
        let mut parts = out.splitn(2, '\t');
        let sid = parts.next().unwrap_or("").to_string();
        let cwd = parts.next().unwrap_or("").to_string();
        let target = if std::path::Path::new(&cwd).is_dir() { cwd } else { dirs::home_dir().unwrap_or_default().to_string_lossy().into_owned() };
        std::env::set_current_dir(&target)?;
        // execvp : remplace le process par claude --resume
        let err = std::process::Command::new("claude").args(["--resume", &sid]).exec();
        return Err(err.into());
    }
    Ok(())
}
```

- [ ] **Step 2: Build complet**

Run: `cargo build`
Expected: compile (ajuster l'API skim si nécessaire, cf. note).

- [ ] **Step 3: Test fonctionnel manuel**

Run: `cargo run`
Expected: liste fuzzy des sessions ; Enter sur une ligne → `claude --resume` dans le bon dossier.
Run: `cargo run -- stats`
Expected: récap par projet.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat(picker): skim + reprise execvp ; CLI complète"
```

---

## Task 12: Packaging Nix (`flake.nix`)

**Files:**
- Create: `~/Documents/temp/cc-sessions/flake.nix`

- [ ] **Step 1: Écrire `flake.nix`**

```nix
{
  description = "cc-sessions — picker des sessions Claude Code (cs)";
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  outputs = { self, nixpkgs }:
    let
      systems = [ "aarch64-darwin" "x86_64-linux" "aarch64-linux" ];
      forAll = nixpkgs.lib.genAttrs systems;
    in {
      packages = forAll (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "cc-sessions";
            version = "0.1.0";
            src = ./.;
            cargoLock.lockFile = ./Cargo.lock;
          };
        });
    };
}
```

- [ ] **Step 2: Générer le lockfile et builder**

```bash
cd ~/Documents/temp/cc-sessions
cargo generate-lockfile
git add -A && git commit -m "build(nix): flake buildRustPackage"
nix build
```

Expected: `result/bin/cs` produit.

- [ ] **Step 3: Vérifier le binaire nixé**

Run: `./result/bin/cs stats`
Expected: récap par projet.

---

## Task 13: Intégration dans le flake `infra` + slash command

**Files:**
- Modify: `infra/flake.nix` (ajouter l'input `cc-sessions`)
- Modify: `infra/hosts/macbook/configuration.nix` (systemPackages + retrait de `fzf`)
- Modify: `~/.claude/commands/note.md` (pointer vers `cs note`)

- [ ] **Step 1: Ajouter l'input dans `infra/flake.nix`**

Dans le bloc `inputs` :

```nix
cc-sessions.url = "path:/Users/lambda/Documents/temp/cc-sessions";
```

(et ajouter `cc-sessions` aux arguments de `outputs`)

- [ ] **Step 2: Exposer le paquet au host macbook**

Dans `hosts/macbook/configuration.nix`, remplacer la ligne `fzf` (ajoutée pour le prototype) par le binaire Rust :

```nix
  environment.systemPackages = [
    pkgs.qbittorrent
    cc-sessions.packages.${pkgs.system}.default  # binaire `cs`
```

(supprimer `pkgs.fzf # picker des sessions Claude Code` — skim est intégré)
Faire remonter `cc-sessions` via `specialArgs`/`_module.args` selon le câblage existant du flake (vérifier comment les autres inputs sont passés aux modules).

- [ ] **Step 3: Mettre `/note` sur le binaire**

Dans `~/.claude/commands/note.md`, la ligne d'exécution devient :

```
cs note "<la note que tu as rédigée>"
```

et `allowed-tools: Bash(cs:*)`.

- [ ] **Step 4: Commit + rebuild signé**

```bash
cd /Users/lambda/Documents/temp/infra
git add flake.nix flake.lock hosts/macbook/configuration.nix
SSH_AUTH_SOCK= git commit -m "feat(darwin): intègre cc-sessions (cs), retire fzf"   # toucher YubiKey
./scripts/safe-rebuild.sh macbook switch
```

- [ ] **Step 5: Vérifier le déploiement**

Run: `cs stats`
Expected: fonctionne sans `cargo`/`nix run` (binaire dans le PATH système).

- [ ] **Step 6: Nettoyage des prototypes**

```bash
rm ~/.local/bin/cs ~/.local/bin/note
```

(Le binaire Rust `cs` les remplace ; `cs note` remplace l'ancien `note` bash.)

---

## Self-Review (effectuée)

**Spec coverage :** picker (T11), reprise execvp (T11), note locale (T5/T8), statuts active/hold/done (T8), stats (T9), done --older-than (T9), archive (T10), purge-archive (T10), cache (mtime,size) (T4), libellé repo/sous-dossier (T3), greenfield (pas de migration — T13 supprime les prototypes), packaging flake + input infra (T12/T13), /note (T13). ✅ Tous couverts.

**Placeholders :** aucun « TBD » ; tout le code est fourni. La seule réserve explicite = API skim à confirmer selon la version (note encadrée T11).

**Cohérence des types :** `Status`, `SessionMeta`, `SessionInfo`, `SessionRow`, `scan::scan_file`, `scan::discover`, `cache::refresh/load/save`, `meta::upsert/load/notes_file`, `project::git_root/label`, `build::rows`, `stats::parse_age_ns/print_stats/mark_done_older_than`, `archive::archive/purge_archive`, `picker::run` — signatures cohérentes d'une tâche à l'autre. `now_stamp` exposé via `now_stamp_pub` pour `stats`.

> **Réserve connue :** l'ordre de compilation impose que `main.rs` (T8) référence des modules écrits en T9–T11 ; les commits intermédiaires de T8–T10 ne compileront pas seuls. Option d'exécution : écrire des stubs vides pour `stats`/`archive`/`picker` en fin de T8 (fns `todo!()`), ou regrouper le commit T8–T11 en un seul. L'exécutant choisit ; recommandation : stubs `todo!()` pour garder des commits qui compilent.
