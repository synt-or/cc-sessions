use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "cs", about = "Picker & gestion des sessions Claude Code")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Écrire/remplacer la note de la session courante
    Note {
        text: Vec<String>,
        /// Ajouter à la note existante
        #[arg(short = 'a', long)]
        append: bool,
    },
    /// Marquer la session courante en attente
    Hold,
    /// Marquer la session courante terminée
    Done {
        /// Marquer en masse les sessions inactives depuis N jours (ex: 30d)
        #[arg(long)]
        older_than: Option<String>,
    },
    /// Réactiver la session courante
    Active,
    /// Statistiques par projet
    Stats,
    /// Archiver des sessions (non destructif)
    Archive {
        #[arg(long)]
        older_than: Option<String>,
        uuids: Vec<String>,
    },
    /// Vider définitivement l'archive (double confirmation)
    PurgeArchive,
}

/// Lit l'identifiant de la session Claude Code courante.
pub fn current_session_id() -> anyhow::Result<String> {
    std::env::var("CLAUDE_CODE_SESSION_ID")
        .map_err(|_| anyhow::anyhow!("CLAUDE_CODE_SESSION_ID absent — lance ceci depuis l'intérieur d'une session (« ! cs note … »)"))
}
