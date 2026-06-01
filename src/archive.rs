use crate::stats::parse_age_ns;
use crate::{build, scan};
use anyhow::Result;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

fn archive_root() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".claude").join("projects-archive")
}

fn projects_root() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".claude").join("projects")
}

/// Déplace les .jsonl ciblés (par âge et/ou uuids) vers l'archive, en conservant
/// l'arborescence relative à ~/.claude/projects.
pub fn archive(projects_dir: &Path, older_than: Option<&str>, uuids: &[String]) -> Result<()> {
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
