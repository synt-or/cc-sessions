use clap::{Parser, Subcommand};

/// Version affichée par `cs --version` : numéro de crate + commit git court,
/// injecté au build par `build.rs` (variable `CS_GIT_COMMIT`).
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("CS_GIT_COMMIT"), ")");

#[derive(Parser)]
#[command(
    name = "cs",
    version = VERSION,
    about = "Picker & gestion des sessions Claude Code",
    after_help = "\
PICKER INTERACTIF (cs sans argument)
  Flèches / texte   Filtrer et naviguer parmi les sessions
  Enter             Ouvrir le sous-menu d'action pour la session sélectionnée
                    → reprendre · active · hold · burn · manual · done · note

  Dans le sous-menu, choisir « Marquer done ✓ » pour clore la session.
  Seul « Reprendre » quitte le picker (lance `claude --resume`) ;
  toutes les autres actions reviennent à la liste filtrée.

  Note : les sessions déjà marquées « done » sont masquées dans le picker.

EXEMPLES
  cs                          Picker interactif
  cs --local                  Picker restreint au repo courant (alias -l)
  cs note \"Résumé de session\" Attacher une note à la session courante
  cs note \"Résumé\" --done      Attacher la note ET marquer la session done
  cs done                     Marquer la session courante done
  cs done --older-than 30d    Marquer done toutes les sessions inactives > 30 jours
  cs hold                     Mettre en attente
  cs active                   Réactiver
  cs stats                    Statistiques par projet
  cs --id <uuid> burn         Marquer UNE session donnée (pas la courante)
"
)]
pub struct Cli {
    /// Inverser l'ordre de tri des sessions dans le picker
    #[arg(long)]
    pub reverse: bool,
    /// Ne lister que les sessions du repo courant (git-root du pwd ;
    /// hors repo, le sous-arbre du pwd)
    #[arg(short = 'l', long)]
    pub local: bool,
    /// Cibler une session par son UUID au lieu de la session courante
    /// (ex: cs --id <uuid> burn)
    #[arg(long, value_name = "UUID", global = true)]
    pub id: Option<String>,
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
        /// Marquer aussi la session done (équivaut à `cs note … && cs done`)
        #[arg(long)]
        done: bool,
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
    /// Marquer la session courante « ready to burn » (🔥 prête à lancer)
    Burn,
    /// Marquer la session courante « needs manual work » (🔍 intervention requise)
    Manual,
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
