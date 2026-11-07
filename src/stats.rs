use crate::model::Status;
use crate::{build, meta};
use anyhow::Result;
use std::collections::BTreeMap;
use crate::paths::Paths;

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
pub fn print_stats(paths: &Paths) -> Result<()> {
    // None : les stats restent globales, indépendantes du pwd.
    let rows = build::all_rows(paths, None);
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
    println!("\n{} sessions.", rows.len());
    Ok(())
}

/// Marque `done` toutes les sessions inactives depuis l'âge donné.
pub fn mark_done_older_than(paths: &Paths, spec: &str) -> Result<()> {
    let age =
        parse_age_ns(spec).ok_or_else(|| anyhow::anyhow!("âge invalide: {spec} (ex: 30d, 12h)"))?;
    let cutoff = now_ns().saturating_sub(age);
    // None délibéré : opération qui ÉCRIT des statuts — son périmètre ne doit
    // jamais dépendre du répertoire depuis lequel `cs done` est lancé.
    let rows = build::all_rows(paths, None);
    let mut n = 0;
    for r in rows
        .iter()
        .filter(|r| r.info.activity_ns() < cutoff && r.status() != Status::Done)
    {
        if let Some(cwd) = &r.info.cwd {
            meta::set_status(paths, cwd, &r.info.session_id, Status::Done)?;
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
