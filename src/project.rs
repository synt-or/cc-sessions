use std::path::Path;
use std::process::Command;

/// Renvoie le git-root d'un répertoire, ou None si hors d'un repo.
pub fn git_root(cwd: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["-C", cwd, "rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if root.is_empty() {
        None
    } else {
        Some(root)
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
        assert_eq!(label("/Users/x/temp/infra", Some("/Users/x/temp/infra")), "infra");
    }

    #[test]
    fn subdir_gives_repo_slash_subdir() {
        assert_eq!(
            label("/Users/x/temp/A2A-COMM/cli", Some("/Users/x/temp/A2A-COMM")),
            "A2A-COMM/cli"
        );
    }

    #[test]
    fn non_git_gives_last_two_segments() {
        assert_eq!(label("/Users/x/email-triage-log/labels", None), "email-triage-log/labels");
    }
}
