use crate::model::SessionRow;
use crate::{cache, meta, project, scan};
use std::collections::HashMap;
use std::path::Path;

const MAX_ROWS: usize = 300;

/// Pipeline complet : découverte -> cache -> méta locale -> filtre -> tri.
///
/// `scope` (cf. `project::scope_root`) restreint la liste aux sessions du
/// périmètre courant — `None` = toutes. Le filtre s'applique **avant** le tri
/// et la troncature à MAX_ROWS : filtrer en aval masquerait les sessions du
/// périmètre qui ne tiennent pas dans le top-300 global.
pub fn rows(projects_dir: &Path, scope: Option<&str>) -> Vec<SessionRow> {
    let files = scan::discover(projects_dir);
    let idx_path = cache::index_path();
    let cached = cache::load(&idx_path);
    let index = cache::refresh(&files, cached);
    let _ = cache::save(&idx_path, &index);

    // caches pour éviter le travail redondant entre lignes du même projet :
    //  - git_root : un subprocess `git` par cwd distinct (au lieu d'un par ligne)
    //  - notes    : un fichier session-notes.jsonl lu une seule fois par projet
    let mut root_cache: HashMap<String, Option<String>> = HashMap::new();
    let mut notes_cache: HashMap<String, HashMap<String, crate::model::SessionMeta>> =
        HashMap::new();

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
            let notes_file = meta::notes_file_with_root(&cwd, root.as_deref());
            let key = notes_file.to_string_lossy().into_owned();
            let metas = notes_cache
                .entry(key)
                .or_insert_with(|| meta::load(&notes_file));
            let m = metas.get(&info.session_id).cloned();
            Some(SessionRow {
                info,
                meta: m,
                project_label: label,
            })
        })
        .collect();

    rows.sort_by_key(|r| r.sort_key());
    rows.truncate(MAX_ROWS);
    rows
}
