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
    let size = meta.len();
    let text = if size > HEAD {
        format!("{}\n{}", read_head(path, HEAD)?, read_tail(path, TAIL)?)
    } else {
        read_head(path, HEAD)?
    };
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
