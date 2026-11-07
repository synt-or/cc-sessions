use std::path::{Path, PathBuf};

/// Tous les emplacements que `cs` lit ou écrit, injectés depuis `main`.
///
/// Sous bac à sable (`CS_SANDBOX=<dossier>`), chaque chemin est sous ce dossier,
/// y compris les notes de session : un essai n'écrit jamais dans un vrai dépôt.
#[derive(Debug, Clone)]
pub struct Paths {
    pub projects: PathBuf,
    pub archive: PathBuf,
    pub cache_index: PathBuf,
    pub state_dir: PathBuf,
    pub registry: PathBuf,
    /// Instantanés de revue : texte de transcript en clair. Sous `state_dir`, hors de
    /// tout dépôt, avec les données de l'utilisateur.
    pub snapshots: PathBuf,
    pub notes_root: Option<PathBuf>,
}

impl Paths {
    pub fn real() -> Paths {
        let home = dirs::home_dir().unwrap_or_default();
        let claude = home.join(".claude");
        let state_dir = home.join(".local").join("state").join("cc-sessions");
        let snapshots = state_dir.join("snapshots");
        Paths {
            projects: claude.join("projects"),
            archive: claude.join("projects-archive"),
            cache_index: dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("cc-sessions")
                .join("index.json"),
            // `dirs::state_dir()` vaut None sur macOS.
            state_dir,
            registry: claude.join("sessions"),
            snapshots,
            notes_root: None,
        }
    }

    pub fn sandbox(root: &Path) -> Paths {
        Paths {
            projects: root.join("projects"),
            archive: root.join("projects-archive"),
            cache_index: root.join("cache").join("index.json"),
            state_dir: root.join("state"),
            registry: root.join("sessions"),
            snapshots: root.join("snapshots"),
            notes_root: Some(root.join("notes")),
        }
    }

    /// À n'appeler que depuis `main` : les tests injectent leurs chemins. Une
    /// variable vide est une erreur : un essai ne doit jamais retomber sur les
    /// vraies données parce qu'un chemin n'était pas défini.
    pub fn from_env() -> anyhow::Result<Paths> {
        match std::env::var_os("CS_SANDBOX") {
            Some(root) if root.is_empty() => anyhow::bail!("CS_SANDBOX est vide"),
            Some(root) => Ok(Paths::sandbox(Path::new(&root))),
            None => Ok(Paths::real()),
        }
    }

    pub fn is_sandbox(&self) -> bool {
        self.notes_root.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_redirects_every_path() {
        let root = Path::new("/bac");
        let p = Paths::sandbox(root);
        assert!(p.is_sandbox());
        for path in [
            &p.projects,
            &p.archive,
            &p.cache_index,
            &p.state_dir,
            &p.registry,
            &p.snapshots,
            p.notes_root.as_ref().unwrap(),
        ] {
            assert!(path.starts_with(root), "{path:?} hors du bac à sable");
        }
    }

    #[test]
    fn real_paths_are_not_sandboxed() {
        assert!(!Paths::real().is_sandbox());
    }
}
