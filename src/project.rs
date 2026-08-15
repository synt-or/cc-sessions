use std::path::Path;

/// Renvoie le git-root d'un répertoire, ou None si hors d'un repo.
/// Remontée de répertoires cherchant `.git` (dossier, ou fichier pour les
/// worktrees/submodules) — pas de subprocess `git` : appelé une fois par ligne
/// du picker, un spawn (~15 ms) par appel rendait l'affichage multi-seconde.
pub fn git_root(cwd: &str) -> Option<String> {
    // canonicalize valide l'existence (parité avec l'échec de `git -C <absent>`)
    // et rend le chemin absolu, sans quoi la remontée par parent() s'arrêterait
    // au premier segment relatif.
    let start = std::fs::canonicalize(cwd).ok()?;
    let mut dir: Option<&Path> = Some(&start);
    while let Some(d) = dir {
        if d.join(".git").exists() {
            return Some(d.to_string_lossy().into_owned());
        }
        dir = d.parent();
    }
    None
}

/// Périmètre « local » d'un répertoire (référence du filtre `cs --local`) :
/// son git-root si on est dans un repo, sinon le répertoire lui-même.
/// Toujours canonicalisé — comparer un `/tmp/x` à un `/private/tmp/x` (macOS)
/// donnerait une liste vide sans la moindre erreur.
pub fn scope_root(dir: &str) -> Option<String> {
    git_root(dir).or_else(|| {
        std::fs::canonicalize(dir)
            .ok()
            .map(|p| p.to_string_lossy().into_owned())
    })
}

/// Une session appartient-elle au périmètre `scope` (issu de `scope_root`) ?
///
/// - `cwd` : répertoire de la session, absent de certains JSONL.
/// - `session_root` : son git-root, déjà résolu (et mémoïsé) par `build::rows`.
///   Vaut `None` si la session est hors repo git **ou** si son cwd a disparu
///   du disque (`canonicalize` échoue → la remontée ne démarre pas).
/// - `scope` : chemin canonique du périmètre courant.
pub fn is_local(cwd: Option<&str>, session_root: Option<&str>, scope: &str) -> bool {
    let Some(cwd) = cwd else { return false };
    match session_root {
        // Deux chemins canoniques du même repo sont identiques : égalité stricte.
        Some(root) => root == scope,
        // Hors repo (ou cwd disparu du disque) : repli lexical. `Path::starts_with`
        // compare par composants — /…/infra-old ne matche pas le scope /…/infra,
        // contrairement à `str::starts_with`.
        None => Path::new(cwd).starts_with(scope),
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
        assert_eq!(
            label("/Users/x/temp/infra", Some("/Users/x/temp/infra")),
            "infra"
        );
    }

    #[test]
    fn subdir_gives_repo_slash_subdir() {
        assert_eq!(
            label("/Users/x/temp/A2A-COMM/cli", Some("/Users/x/temp/A2A-COMM")),
            "A2A-COMM/cli"
        );
    }

    #[test]
    fn git_root_walks_up_to_dot_git() {
        let base = std::env::temp_dir().join("cs_project_test_walk");
        let sub = base.join("repo").join("src").join("deep");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(base.join("repo").join(".git")).unwrap();
        let root = git_root(&sub.to_string_lossy()).unwrap();
        // canonicalize peut préfixer /private sur macOS → comparer les fins
        assert!(root.ends_with("cs_project_test_walk/repo"), "{root}");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn git_root_none_when_no_repo_or_missing_dir() {
        assert_eq!(git_root("/nonexistent/path/xyz"), None);
    }

    #[test]
    fn git_root_accepts_dot_git_file_worktree() {
        let base = std::env::temp_dir().join("cs_project_test_wt");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join(".git"), "gitdir: /elsewhere\n").unwrap();
        let root = git_root(&base.to_string_lossy()).unwrap();
        assert!(root.ends_with("cs_project_test_wt"), "{root}");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn scope_root_is_the_repo_inside_a_git_tree() {
        let base = std::env::temp_dir().join("cs_project_test_scope");
        let sub = base.join("repo").join("src");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(base.join("repo").join(".git")).unwrap();
        let scope = scope_root(&sub.to_string_lossy()).unwrap();
        assert!(scope.ends_with("cs_project_test_scope/repo"), "{scope}");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn scope_root_falls_back_to_the_dir_outside_a_repo() {
        let base = std::env::temp_dir().join("cs_project_test_scope_nogit");
        std::fs::create_dir_all(&base).unwrap();
        let scope = scope_root(&base.to_string_lossy()).unwrap();
        assert!(scope.ends_with("cs_project_test_scope_nogit"), "{scope}");
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn scope_root_none_when_dir_missing() {
        assert_eq!(scope_root("/nonexistent/path/xyz"), None);
    }

    // --- is_local : contrat du filtre `cs --local` ---

    #[test]
    fn same_repo_root_is_local() {
        assert!(is_local(
            Some("/Users/x/temp/infra"),
            Some("/Users/x/temp/infra"),
            "/Users/x/temp/infra"
        ));
    }

    #[test]
    fn subdir_of_same_repo_is_local() {
        assert!(is_local(
            Some("/Users/x/temp/infra/hosts/macbook"),
            Some("/Users/x/temp/infra"),
            "/Users/x/temp/infra"
        ));
    }

    #[test]
    fn other_repo_is_not_local() {
        assert!(!is_local(
            Some("/Users/x/temp/cc-sessions"),
            Some("/Users/x/temp/cc-sessions"),
            "/Users/x/temp/infra"
        ));
    }

    #[test]
    fn missing_cwd_is_not_local() {
        assert!(!is_local(None, None, "/Users/x/temp/infra"));
    }

    #[test]
    fn rootless_session_under_scope_is_local() {
        // hors repo git, ou cwd disparu du disque : repli lexical sur le préfixe
        assert!(is_local(
            Some("/Users/x/temp/infra/doc"),
            None,
            "/Users/x/temp/infra"
        ));
    }

    #[test]
    fn rootless_session_outside_scope_is_not_local() {
        assert!(!is_local(Some("/Users/x/other"), None, "/Users/x/temp/infra"));
    }

    #[test]
    fn sibling_with_common_prefix_is_not_local() {
        // /Users/x/temp/infra-old ne doit PAS matcher le scope /Users/x/temp/infra
        assert!(!is_local(
            Some("/Users/x/temp/infra-old"),
            Some("/Users/x/temp/infra-old"),
            "/Users/x/temp/infra"
        ));
        assert!(!is_local(
            Some("/Users/x/temp/infra-old/doc"),
            None,
            "/Users/x/temp/infra"
        ));
    }

    #[test]
    fn non_git_gives_last_two_segments() {
        assert_eq!(
            label("/Users/x/email-triage-log/labels", None),
            "email-triage-log/labels"
        );
    }
}
