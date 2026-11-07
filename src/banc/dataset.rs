//! Les cas du banc : un instantané et la catégorie choisie par l'utilisateur.
//!
//! Seul le fichier figé compte : rien n'est recalculé depuis un transcript, et la
//! durée du bandeau reste celle qu'il avait à l'écran.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

/// Signale un jeu synthétique, pour que le rapport le dise.
pub const SYNTHETIC_MARKER: &str = "SYNTHETIC";

#[derive(Debug, Clone, PartialEq)]
pub struct Case {
    pub session_id: String,
    pub snapshot_hash: String,
    pub text: String,
    pub truth: String,
    pub has_prior_flag: bool,
    /// Statut annoncé par le bandeau, s'il y en a un.
    pub prior_status: Option<String>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Skipped {
    pub missing_snapshot: u32,
    pub bad_hash: u32,
    pub unknown_category: u32,
    pub bad_id: u32,
    pub unreadable_lines: u32,
    /// Transcript illisible à l'ouverture : l'instantané ne montre pas la session.
    pub read_error: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Dataset {
    pub cases: Vec<Case>,
    pub skipped: Skipped,
    pub synthetic: bool,
}

pub const SYNTHETIC_PREFIX: &str = "synth-";

/// Le dossier d'état de `cs` : `~/.local/state/cc-sessions`. Il contient le vrai tri.
pub fn real_state_dir() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".local/state/cc-sessions"))
}

/// Un chemin tel qu'il est vraiment sur le disque : le plus long ancêtre existant est
/// résolu (liens compris), et le reste y est rattaché. `None` pour un `..` : son sens
/// dépend de ce qui existe, donc aucun contrôle ne peut s'y fier.
pub fn resolved(path: &Path) -> Option<PathBuf> {
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let mut existing = absolute.as_path();
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(existing) {
            return Some(rest.iter().rev().fold(real, |p: PathBuf, part| p.join(part)));
        }
        // Un lien pendant ne doit pas passer pour un dossier à créer.
        if existing.symlink_metadata().is_ok() {
            return None;
        }
        rest.push(existing.file_name()?.to_os_string());
        existing = existing.parent()?;
    }
}

fn identity(path: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.dev(), meta.ino()))
}

/// Le plus proche ancêtre existant d'un chemin résolu, et ce qui reste en dessous.
fn nearest_existing(path: &Path) -> Option<((u64, u64), Vec<String>)> {
    let mut rest = Vec::new();
    let mut current = path;
    loop {
        if let Some(id) = identity(current) {
            rest.reverse();
            return Some((id, rest));
        }
        rest.push(current.file_name()?.to_string_lossy().to_lowercase());
        current = current.parent()?;
    }
}

/// Le dossier d'état d'origine ou l'un de ses sous-dossiers ? Comparé par identité sur
/// le disque, pas par nom : `/System/Volumes/Data/Users/…` désigne le même dossier que
/// `/Users/…` et `realpath` garde les deux formes. Vaut aussi quand l'état n'existe pas
/// encore, par son plus proche ancêtre. APFS ignore la casse, la comparaison aussi. Un
/// chemin qu'on ne sait pas résoudre compte comme interdit.
fn is_inside(dir: &Path, state: &Path) -> bool {
    let (Some(dir), Some(state)) = (resolved(dir), resolved(state)) else {
        return true;
    };
    let Some((state_id, state_rest)) = nearest_existing(&state) else {
        return true;
    };
    let parts: Vec<String> = dir
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    let mut ancestor = PathBuf::new();
    for (index, component) in dir.components().enumerate() {
        ancestor.push(component);
        if identity(&ancestor) == Some(state_id) && parts[index + 1..].starts_with(&state_rest) {
            return true;
        }
    }
    false
}

/// Le chemin désigne-t-il le vrai dossier d'état ?
pub fn is_real_state(dir: &Path) -> bool {
    real_state_dir().is_none_or(|state| is_inside(dir, &state))
}

/// Un identifiant sert de composant de chemin.
fn safe_component(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// La dernière décision de chaque session, après application des annulations.
fn truths(log: &str, skipped: &mut Skipped) -> Vec<(String, String, String)> {
    let mut stacks: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for line in log.lines() {
        let parsed = serde_json::from_str::<Value>(line).ok();
        let Some((v, sid)) = parsed.as_ref().and_then(|v| Some((v, v.get("sessionId")?.as_str()?))) else {
            // Une annulation illisible ferait revivre une décision : ça doit se voir.
            if !line.trim().is_empty() {
                skipped.unreadable_lines += 1;
            }
            continue;
        };
        match v.get("event").and_then(Value::as_str) {
            Some("decision") => {
                let category = v.get("category").and_then(Value::as_str).unwrap_or_default();
                let hash = v.get("snapshotHash").and_then(Value::as_str).unwrap_or_default();
                if !stacks.contains_key(sid) {
                    order.push(sid.to_string());
                }
                stacks
                    .entry(sid.to_string())
                    .or_default()
                    .push((category.to_string(), hash.to_string()));
            }
            Some("undo") => {
                if let Some(stack) = stacks.get_mut(sid) {
                    stack.pop();
                }
            }
            _ => {}
        }
    }
    order
        .into_iter()
        .filter_map(|sid| {
            let (category, hash) = stacks.get(&sid)?.last()?.clone();
            Some((sid, category, hash))
        })
        .collect()
}

/// Le bandeau d'une session déjà triée puis reprise : un indice sur le tri passé, que
/// l'utilisateur a lui aussi sous les yeux. La première ligne est le titre, jamais un bandeau.
fn prior_flag(text: &str) -> Option<String> {
    let header = text.split("\n--- en-tête\n").nth(1)?.split("\n\n--- note\n").next()?;
    let line = header.lines().skip(1).find(|l| l.contains(" posé le "))?;
    let label = line.split(" posé le ").next()?;
    let status = match () {
        _ if label.contains("hold") => "hold",
        _ if label.contains("burn") => "ready_to_burn",
        _ if label.contains("manual") => "needs_manual_work",
        _ if label.contains("done") => "done",
        _ => "active",
    };
    Some(status.to_string())
}

pub fn load(dir: &Path) -> Result<Dataset, String> {
    let mut skipped = Skipped::default();
    let log = std::fs::read_to_string(dir.join("review-decisions.jsonl"))
        .map_err(|e| format!("historique des décisions illisible : {}", e.kind()))?;
    let decided = truths(&log, &mut skipped);
    let root = std::fs::canonicalize(dir.join("snapshots")).ok();
    let mut cases = Vec::new();
    let known = |c: &str| crate::categories::CATEGORIES.iter().any(|(k, _)| *k == c);
    for (sid, category, hash) in decided {
        if !safe_component(&sid) || !safe_component(&hash) {
            skipped.bad_id += 1;
            continue;
        }
        if !known(&category) {
            skipped.unknown_category += 1;
            continue;
        }
        let path = dir.join("snapshots").join(&sid).join(format!("{hash}.txt"));
        // Un lien symbolique ne doit pas faire lire un fichier hors du jeu.
        let inside = std::fs::canonicalize(&path)
            .ok()
            .zip(root.as_ref())
            .is_some_and(|(p, r)| p.starts_with(r));
        let Some(text) = inside.then(|| std::fs::read_to_string(&path).ok()).flatten() else {
            skipped.missing_snapshot += 1;
            continue;
        };
        if format!("{:x}", Sha256::digest(text.as_bytes())) != hash {
            skipped.bad_hash += 1;
            continue;
        }
        if text.lines().any(|l| l == "read_error: true") {
            skipped.read_error += 1;
            continue;
        }
        // Le fichier doit bien appartenir à cette session.
        if text.lines().find_map(|l| l.strip_prefix("session: ")).is_some_and(|s| s != sid) {
            skipped.bad_id += 1;
            continue;
        }
        let prior_status = prior_flag(&text);
        cases.push(Case {
            has_prior_flag: prior_status.is_some(),
            prior_status,
            session_id: sid,
            snapshot_hash: hash,
            text,
            truth: category,
        });
    }
    Ok(Dataset {
        cases,
        skipped,
        synthetic: dir.join(SYNTHETIC_MARKER).exists(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str, synthetic: bool) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("cs_banc_dataset_{name}"));
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(d.join("snapshots")).unwrap();
        if synthetic {
            std::fs::write(d.join(SYNTHETIC_MARKER), "").unwrap();
        }
        d
    }

    fn hash(text: &str) -> String {
        format!("{:x}", Sha256::digest(text.as_bytes()))
    }

    fn snapshot(d: &Path, sid: &str, text: &str) -> String {
        let h = hash(text);
        let s = d.join("snapshots").join(sid);
        std::fs::create_dir_all(&s).unwrap();
        std::fs::write(s.join(format!("{h}.txt")), text).unwrap();
        h
    }

    fn decision(sid: &str, category: &str, h: &str) -> String {
        format!("{{\"sessionId\":\"{sid}\",\"event\":\"decision\",\"category\":\"{category}\",\"snapshotHash\":\"{h}\"}}\n")
    }

    fn undo(sid: &str) -> String {
        format!("{{\"sessionId\":\"{sid}\",\"event\":\"undo\",\"category\":\"hold\"}}\n")
    }

    #[test]
    fn real_state_dir_is_recognized() {
        let state = real_state_dir().unwrap();
        assert!(is_real_state(&state));
        assert!(is_real_state(&state.join("sous-dossier")));
        assert!(!is_real_state(Path::new("/tmp/ailleurs")));
    }

    #[test]
    fn every_way_of_naming_the_state_dir_is_refused() {
        let root = std::env::temp_dir().join("cs_banc_paths");
        std::fs::remove_dir_all(&root).ok();
        let state = root.join("etat");
        std::fs::create_dir_all(&state).unwrap();
        std::os::unix::fs::symlink(&root, root.join("lien")).unwrap();
        assert!(is_inside(&state, &state));
        assert!(is_inside(&state.join("x/y"), &state));
        assert!(is_inside(&root.join("absent/../etat"), &state), "un .. est refusé");
        assert!(is_inside(&root.join("lien/etat"), &state), "par un lien");
        assert!(is_inside(&root.join("lien/etat/x"), &state));
        assert!(is_inside(&root.join("ETAT/x"), &state), "autre casse");
        assert!(!is_inside(&root.join("ailleurs"), &state));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn firmlink_alias_of_the_state_dir_is_refused() {
        let data_volume = Path::new("/System/Volumes/Data");
        if !data_volume.is_dir() {
            return;
        }
        let root = std::fs::canonicalize(std::env::temp_dir()).unwrap().join("cs_banc_firmlink");
        std::fs::remove_dir_all(&root).ok();
        std::fs::create_dir_all(&root).unwrap();
        let alias = data_volume.join(root.strip_prefix("/").unwrap());
        if !alias.is_dir() {
            return; // pas d'alias dans ce bac à sable de build
        }
        // État existant, puis état pas encore créé : le cas d'aujourd'hui.
        let state = root.join("etat");
        std::fs::create_dir_all(&state).unwrap();
        assert!(is_inside(&alias.join("etat"), &state));
        assert!(is_inside(&alias.join("etat/x"), &state));
        let absent = root.join("pas-encore/etat");
        assert!(is_inside(&alias.join("pas-encore/etat"), &absent));
        assert!(is_inside(&alias.join("PAS-ENCORE/ETAT/x"), &absent));
        assert!(!is_inside(&alias.join("pas-encore/autre"), &absent));
        assert!(!is_inside(&alias.join("ailleurs"), &state));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn read_error_snapshot_is_not_a_case() {
        let d = dir("read_error", true);
        let a = snapshot(&d, "synth-a", "session: synth-a\nread_error: true\n");
        std::fs::write(d.join("review-decisions.jsonl"), decision("synth-a", "hold", &a)).unwrap();
        let data = load(&d).unwrap();
        assert!(data.cases.is_empty());
        assert_eq!(data.skipped.read_error, 1);
    }

    #[test]
    fn snapshot_of_another_session_is_skipped() {
        let d = dir("other", true);
        let a = snapshot(&d, "synth-a", "session: 6aa6feb6\n");
        std::fs::write(d.join("review-decisions.jsonl"), decision("synth-a", "hold", &a)).unwrap();
        assert_eq!(load(&d).unwrap().skipped.bad_id, 1);
    }

    #[test]
    fn path_in_session_id_is_skipped() {
        let d = dir("traversal", true);
        let outside = d.join("dehors.txt");
        std::fs::write(&outside, "secret").unwrap();
        let h = hash("secret");
        std::fs::write(
            d.join("review-decisions.jsonl"),
            decision("synth-../../dehors", "hold", &h) + &decision(&format!("synth-{}", outside.display()), "hold", &h),
        )
        .unwrap();
        let data = load(&d).unwrap();
        assert!(data.cases.is_empty());
        assert_eq!(data.skipped.bad_id, 2);
    }

    #[test]
    fn symlink_out_of_the_dataset_is_skipped() {
        let d = dir("symlink", true);
        let outside = d.join("dehors.txt");
        std::fs::write(&outside, "secret").unwrap();
        let h = hash("secret");
        let s = d.join("snapshots/synth-a");
        std::fs::create_dir_all(&s).unwrap();
        std::os::unix::fs::symlink(&outside, s.join(format!("{h}.txt"))).unwrap();
        std::fs::write(d.join("review-decisions.jsonl"), decision("synth-a", "hold", &h)).unwrap();
        let data = load(&d).unwrap();
        assert!(data.cases.is_empty());
        assert_eq!(data.skipped.missing_snapshot, 1);
    }

    #[test]
    fn unreadable_log_line_is_counted() {
        let d = dir("unreadable", true);
        let a = snapshot(&d, "synth-a", "aa");
        std::fs::write(d.join("review-decisions.jsonl"), decision("synth-a", "hold", &a) + "{\"sessionId\":\"synth-a\",\"ev\n").unwrap();
        assert_eq!(load(&d).unwrap().skipped.unreadable_lines, 1);
    }

    #[test]
    fn undone_decision_is_not_truth() {
        let d = dir("undo", true);
        let a = snapshot(&d, "synth-a", "aa");
        let b = snapshot(&d, "synth-b", "bb");
        let log = format!(
            "{}{}{}{}{}",
            decision("synth-a", "hold", &a),
            undo("synth-a"),
            decision("synth-b", "hold", &b),
            undo("synth-b"),
            decision("synth-b", "done", &b)
        );
        std::fs::write(d.join("review-decisions.jsonl"), log).unwrap();
        let data = load(&d).unwrap();
        assert_eq!(data.cases.len(), 1);
        assert_eq!((data.cases[0].session_id.as_str(), data.cases[0].truth.as_str()), ("synth-b", "done"));
    }

    #[test]
    fn hash_mismatch_is_skipped_and_counted() {
        let d = dir("badhash", true);
        let a = snapshot(&d, "synth-a", "aa");
        std::fs::write(d.join("snapshots/synth-a").join(format!("{a}.txt")), "modifié").unwrap();
        std::fs::write(d.join("review-decisions.jsonl"), decision("synth-a", "hold", &a)).unwrap();
        let data = load(&d).unwrap();
        assert!(data.cases.is_empty());
        assert_eq!(data.skipped.bad_hash, 1);
    }

    #[test]
    fn missing_snapshot_is_skipped_and_counted() {
        let d = dir("missing", true);
        std::fs::write(d.join("review-decisions.jsonl"), decision("synth-a", "hold", "absent")).unwrap();
        assert_eq!(load(&d).unwrap().skipped.missing_snapshot, 1);
    }

    #[test]
    fn undecided_session_is_never_a_case() {
        let d = dir("undecided", true);
        snapshot(&d, "vue-seulement", "aa");
        std::fs::write(d.join("review-decisions.jsonl"), "").unwrap();
        assert!(load(&d).unwrap().cases.is_empty());
    }

    #[test]
    fn prior_flag_is_detected_only_on_a_banner_line() {
        let text = |head: &str| format!("x\n--- en-tête\n{head}\n\n--- note\n| il a été posé le 12/10");
        assert_eq!(prior_flag(&text("| repo · titre\n| ⏳ hold posé le 12/10")).as_deref(), Some("hold"));
        assert_eq!(prior_flag(&text("| repo · titre\n| 🔥 burn posé le 12/10")).as_deref(), Some("ready_to_burn"));
        assert_eq!(prior_flag(&text("| repo · titre")), None);
        assert_eq!(prior_flag(&text("| repo · Bandeau posé le matin\n| dernier message")), None);
    }

    #[test]
    fn banner_duration_comes_from_the_frozen_file() {
        let d = dir("banner", true);
        let text = "v1\n--- en-tête\n| repo\n| ⏳ hold posé le 12/10 à 14:32\n| Depuis : 7 tours · 2 h 15 de travail\n\n--- note\n| x\n";
        let a = snapshot(&d, "synth-a", text);
        std::fs::write(d.join("review-decisions.jsonl"), decision("synth-a", "hold", &a)).unwrap();
        let data = load(&d).unwrap();
        assert!(data.cases[0].text.contains("2 h 15 de travail"));
        assert!(data.cases[0].has_prior_flag);
    }

    #[test]
    fn unknown_category_is_skipped() {
        let d = dir("unknown", true);
        let a = snapshot(&d, "synth-a", "aa");
        std::fs::write(d.join("review-decisions.jsonl"), decision("synth-a", "autre", &a)).unwrap();
        assert_eq!(load(&d).unwrap().skipped.unknown_category, 1);
    }
}
