use crate::stats::parse_age_ns;
use crate::{build, scan};
use anyhow::Result;
use std::io::{self, Write};
use crate::paths::Paths;
use std::collections::HashMap;
use std::path::Path;

/// Déplace les .jsonl ciblés (par âge et/ou uuids) vers l'archive, en conservant
/// l'arborescence relative à ~/.claude/projects.
pub fn archive(paths: &Paths, older_than: Option<&str>, uuids: &[String]) -> Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_nanos() as u64;
    let cutoff = older_than
        .and_then(parse_age_ns)
        .map(|a| now.saturating_sub(a));
    // Date du dernier message : le fichier d'une session ouverte bouge tout seul.
    let activity: HashMap<_, _> = build::all_rows(paths, None)
        .into_iter()
        .map(|r| (r.info.path.clone(), r.info.activity_ns()))
        .collect();
    let files = scan::discover(&paths.projects);
    let mut moved = 0;
    for f in files {
        let sid = f
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let by_uuid = uuids.iter().any(|u| u == &sid);
        let by_age = cutoff.is_some_and(|c| activity.get(&f).is_some_and(|at| *at < c));
        if !(by_uuid || by_age) {
            continue;
        }
        let rel = f.strip_prefix(&paths.projects).unwrap_or(&f);
        let dest = paths.archive.join(rel);
        if let Some(d) = dest.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::rename(&f, &dest)?;
        moved += 1;
    }
    let _ = build::all_rows(paths, None);
    println!(
        "✓ {moved} session(s) archivée(s) dans {}",
        paths.archive.display()
    );
    Ok(())
}

/// Supprime définitivement l'archive après double confirmation.
pub fn purge_archive(paths: &Paths) -> Result<()> {
    let root = paths.archive.clone();
    if !root.exists() {
        println!("Archive vide, rien à purger.");
        return Ok(());
    }
    let count = walk_count(&root);
    println!(
        "⚠️  Suppression DÉFINITIVE de {count} fichier(s) dans {}",
        root.display()
    );
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

fn walk_count(dir: &Path) -> usize {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_than_uses_last_message_not_file_date() {
        let root = std::env::temp_dir().join("cs_archive_activity");
        std::fs::remove_dir_all(&root).ok();
        let paths = Paths::sandbox(&root);
        let dir = paths.projects.join("-repo");
        std::fs::create_dir_all(&dir).unwrap();
        let body = |ts: &str| {
            format!(
                "{{\"type\":\"user\",\"cwd\":\"/repo\",\"timestamp\":\"{ts}\",\"message\":{{\"content\":\"x\"}}}}\n{}\n",
                " ".repeat(2100)
            )
        };
        // Fichiers tout juste écrits, dates de dernier message différentes.
        std::fs::write(dir.join("vieille.jsonl"), body("2020-01-01T00:00:00.000Z")).unwrap();
        std::fs::write(dir.join("recente.jsonl"), body("2999-01-01T00:00:00.000Z")).unwrap();
        archive(&paths, Some("30d"), &[]).unwrap();
        assert!(paths.archive.join("-repo/vieille.jsonl").exists());
        assert!(dir.join("recente.jsonl").exists());
        std::fs::remove_dir_all(&root).ok();
    }
}
