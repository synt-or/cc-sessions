use crate::model::SessionRow;
use crate::{cache, meta, project, queue, scan};
use std::collections::HashMap;
use crate::paths::Paths;

const MAX_ROWS: usize = 300;

/// Lignes pour le picker et les stats : périmètre (cf. `queue::in_scope`, sauf
/// `all`), puis limite d'affichage. Filtrer avant de tronquer : le flux du pont
/// vocal pousserait sinon les vieilles sessions hors de la liste.
pub fn rows(paths: &Paths, scope: Option<&str>, all: bool) -> Vec<SessionRow> {
    let mut rows: Vec<SessionRow> = all_rows(paths, scope)
        .into_iter()
        .filter(|r| all || queue::in_scope(&r.info))
        .collect();
    rows.truncate(MAX_ROWS);
    rows
}

/// Pipeline complet, sans limite : découverte -> cache -> méta locale -> filtre -> tri.
///
/// `scope` (cf. `project::scope_root`) restreint la liste aux sessions du
/// périmètre courant — `None` = toutes. Le filtre s'applique **avant** le tri
/// et la troncature à MAX_ROWS : filtrer en aval masquerait les sessions du
/// périmètre qui ne tiennent pas dans le top-300 global.
pub fn all_rows(paths: &Paths, scope: Option<&str>) -> Vec<SessionRow> {
    let files = scan::discover(&paths.projects);
    let idx_path = &paths.cache_index;
    let cached = cache::load(idx_path);
    if cached.is_empty() && !files.is_empty() {
        eprintln!("cs : première lecture de {} transcripts…", files.len());
    }
    let index = cache::refresh(&files, cached);
    let _ = cache::save(idx_path, &index);

    // caches pour éviter le travail redondant entre lignes du même projet :
    //  - git_root : un subprocess `git` par cwd distinct (au lieu d'un par ligne)
    //  - notes    : un fichier session-notes.jsonl lu une seule fois par projet
    let mut root_cache: HashMap<String, Option<String>> = HashMap::new();
    let mut notes_cache: HashMap<String, HashMap<String, crate::model::SessionMeta>> =
        HashMap::new();
    let central = meta::load(&meta::central_notes_file(paths));

    let mut rows: Vec<SessionRow> = index
        .into_values()
        .filter_map(|info| {
            let cwd = info.cwd.clone().unwrap_or_default();
            let root = root_cache
                .entry(cwd.clone())
                .or_insert_with(|| project::git_root(&cwd))
                .clone();
            if let Some(scope) = scope {
                if !project::is_local(info.cwd.as_deref(), root.as_deref(), scope) {
                    return None;
                }
            }
            let label = project::label(&cwd, root.as_deref());
            let notes_file = meta::project_notes_file(paths, &cwd, root.as_deref());
            let key = notes_file.to_string_lossy().into_owned();
            let metas = notes_cache
                .entry(key)
                .or_insert_with(|| meta::load(&notes_file));
            let m = meta::most_recent(
                metas.get(&info.session_id).cloned(),
                central.get(&info.session_id).cloned(),
            );
            Some(SessionRow {
                info,
                meta: m,
                project_label: label,
            })
        })
        .collect();

    rows.sort_by_key(|r| r.sort_key());
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_bridge_flood_does_not_push_old_session_out_of_picker() {
        let root = std::env::temp_dir().join("cs_build_flood");
        std::fs::remove_dir_all(&root).ok();
        let paths = Paths::sandbox(&root);
        let line = |cwd: &str, ts: &str| {
            format!(
                "{{\"type\":\"user\",\"cwd\":\"{cwd}\",\"timestamp\":\"{ts}\",\"message\":{{\"content\":\"x\"}}}}\n{}\n",
                " ".repeat(2100)
            )
        };
        let write = |dir: &str, sid: &str, body: &str| {
            let d = paths.projects.join(dir);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(format!("{sid}.jsonl")), body).unwrap();
        };
        write("-old-repo", "ancienne", &line("/old/repo", "2020-01-01T00:00:00.000Z"));
        for i in 0..(MAX_ROWS + 5) {
            write(
                &format!("-T-claude-voice-msg-side-{i}"),
                &format!("pont{i}"),
                &line(&format!("/T/claude-voice-msg-side-{i}"), "2026-11-07T10:00:00.000Z"),
            );
        }
        let visible = rows(&paths, None, false);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].info.session_id, "ancienne");
        assert_eq!(rows(&paths, None, true).len(), MAX_ROWS);
        assert_eq!(all_rows(&paths, None).len(), MAX_ROWS + 6);
        std::fs::remove_dir_all(&root).ok();
    }
}
