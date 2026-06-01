use crate::model::Status;
use crate::{build, meta};
use anyhow::Result;
use std::collections::BTreeMap;
use std::path::Path;

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
pub fn print_stats(projects_dir: &Path) -> Result<()> {
    let rows = build::rows(projects_dir);
    let mut by_proj: BTreeMap<String, (usize, u64, u64)> = BTreeMap::new();
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
pub fn mark_done_older_than(projects_dir: &Path, spec: &str) -> Result<()> {
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
