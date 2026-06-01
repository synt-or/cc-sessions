use crate::model::SessionRow;
use crate::{cache, meta, project, scan};
use std::collections::HashMap;
use std::path::Path;

const MAX_ROWS: usize = 300;

/// Pipeline complet : découverte -> cache -> méta locale -> tri.
pub fn rows(projects_dir: &Path) -> Vec<SessionRow> {
    let files = scan::discover(projects_dir);
    let idx_path = cache::index_path();
    let cached = cache::load(&idx_path);
    let index = cache::refresh(&files, cached);
    let _ = cache::save(&idx_path, &index);

    // caches pour éviter le travail redondant entre lignes du même projet :
    //  - git_root : un subprocess `git` par cwd distinct (au lieu d'un par ligne)
    //  - notes    : un fichier session-notes.jsonl lu une seule fois par projet
    let mut root_cache: HashMap<String, Option<String>> = HashMap::new();
    let mut notes_cache: HashMap<String, HashMap<String, crate::model::SessionMeta>> = HashMap::new();

    let mut rows: Vec<SessionRow> = index
        .into_values()
        .map(|info| {
            let cwd = info.cwd.clone().unwrap_or_default();
            let root = root_cache
                .entry(cwd.clone())
                .or_insert_with(|| project::git_root(&cwd))
                .clone();
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
