//! Jeu synthétique au format réel des instantanés. Aucun contenu réel.
use crate::categories::CATEGORIES;
use crate::dataset::{is_real_state, resolved, SYNTHETIC_MARKER, SYNTHETIC_PREFIX};
use std::io::Write;
use sha2::{Digest, Sha256};
use std::path::Path;

/// Générateur déterministe, pour des essais reproductibles.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Uniforme dans `[0, 1[`.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

const PHRASES: [(&str, &str); 5] = [
    ("active", "Je continue le chantier fictif demain matin, la prochaine étape est claire."),
    ("hold", "Chantier fictif en pause, à reprendre le mois prochain."),
    ("ready_to_burn", "Le plan fictif est prêt, il reste à le lancer quand le quota revient."),
    ("needs_manual_work", "Il faut brancher le câble fictif à la main avant de continuer."),
    ("done", "Le chantier fictif est terminé et poussé, plus rien à faire."),
];

/// Écrit `n` cas, plus du bruit : décisions annulées et instantanés sans décision.
///
/// Refuse un dossier existant non vide et le dossier d'état réel : écraser
/// `review-decisions.jsonl` y détruirait le tri de l'utilisateur, sans retour.
pub fn dataset(seed: u64, n: usize, dir: &Path) -> std::io::Result<()> {
    if resolved(dir).is_none() {
        return Err(std::io::Error::other("chemin avec .. ou lien pendant : refus"));
    }
    if is_real_state(dir) {
        return Err(std::io::Error::other("le dossier d'état réel est refusé"));
    }
    if std::fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some()) {
        return Err(std::io::Error::other("le dossier existe et n'est pas vide : refus"));
    }
    // Dernier filet : ces deux fichiers ne peuvent pas en écraser d'existants.
    let create_new = |name: &str| std::fs::OpenOptions::new().create_new(true).write(true).open(dir.join(name));
    let mut rng = Rng::new(seed);
    std::fs::create_dir_all(dir.join("snapshots"))?;
    let mut decisions = {
        create_new(SYNTHETIC_MARKER)?;
        create_new("review-decisions.jsonl")?
    };
    let mut log = String::new();
    let write_snapshot = |sid: &str, text: &str| -> std::io::Result<String> {
        let hash = format!("{:x}", Sha256::digest(text.as_bytes()));
        let d = dir.join("snapshots").join(sid);
        std::fs::create_dir_all(&d)?;
        std::fs::write(d.join(format!("{hash}.txt")), text)?;
        Ok(hash)
    };
    for i in 0..n {
        let sid = format!("{SYNTHETIC_PREFIX}{i:05}");
        // Répartition inégale, comme un vrai tri.
        let category = PHRASES[match rng.below(10) {
            0..=3 => 1,
            4..=6 => 4,
            7 => 0,
            8 => 2,
            _ => 3,
        }];
        let flagged = rng.below(4) == 0;
        let header = if flagged {
            let status = ["active", "⏳ hold", "✓ done", "🔥 burn", "🔍 manual"][rng.below(5)];
            format!(
                "| projet-fictif-{i} · Titre fictif\n| {status} posé le 12/10 à 14:32\n| Depuis : {} tours · 1 h 05 de travail · dernier message le 15/10 à 09:40",
                1 + rng.below(9)
            )
        } else {
            format!("| projet-fictif-{i} · Titre fictif\n| dernier message le 15/10 à 09:40")
        };
        let text = format!(
            "cs-review-snapshot v1\nsession: {sid}\ntranscript_bytes: {}\nread_error: false\n\n--- en-tête\n{header}\n\n--- note\n| —\n\n--- recaps\n| 15/10 09:40  {}\n\n--- échanges\n> Toi · 15/10 09:30\n| Où en est-on ?\n> Claude · 15/10 09:40\n| {}\n",
            1000 + i,
            category.1,
            category.1,
        );
        let hash = write_snapshot(&sid, &text)?;
        if i % 7 == 0 {
            // Le tri a changé d'avis : seule la dernière décision compte.
            log.push_str(&line(&sid, CATEGORIES[rng.below(CATEGORIES.len())].0, &hash));
            log.push_str(&format!("{{\"sessionId\":\"{sid}\",\"event\":\"undo\",\"category\":\"hold\"}}\n"));
        }
        log.push_str(&line(&sid, category.0, &hash));
    }
    write_snapshot(&format!("{SYNTHETIC_PREFIX}vue-sans-decision"), "instantané sans décision\n")?;
    decisions.write_all(log.as_bytes())
}

fn line(sid: &str, category: &str, hash: &str) -> String {
    format!(
        "{{\"sessionId\":\"{sid}\",\"event\":\"decision\",\"category\":\"{category}\",\"at\":\"2026-11-07T10:00:00+01:00\",\"snapshotVersion\":1,\"snapshotHash\":\"{hash}\",\"snapshotStored\":true,\"transcriptBytes\":1000}}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataset::load;

    #[test]
    fn synthetic_dataset_loads() {
        let d = std::env::temp_dir().join("cs_banc_synth");
        std::fs::remove_dir_all(&d).ok();
        dataset(1, 30, &d).unwrap();
        let data = load(&d).unwrap();
        assert_eq!(data.cases.len(), 30);
        assert!(data.synthetic);
        assert_eq!(data.skipped, Default::default());
        assert!(data.cases[0].text.starts_with("cs-review-snapshot v1"));
        assert!(data.cases.iter().any(|c| c.has_prior_flag));
        assert!(data.cases.iter().any(|c| !c.has_prior_flag));
    }

    #[test]
    fn existing_decisions_are_never_overwritten() {
        let d = std::env::temp_dir().join("cs_banc_synth_keep");
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("review-decisions.jsonl"), "le vrai tri\n").unwrap();
        assert!(dataset(1, 3, &d).is_err());
        assert_eq!(std::fs::read_to_string(d.join("review-decisions.jsonl")).unwrap(), "le vrai tri\n");
        assert!(!d.join(SYNTHETIC_MARKER).exists());
    }

    #[test]
    fn dotdot_in_output_is_refused() {
        let d = std::env::temp_dir().join("cs_banc_synth_dotdot");
        std::fs::remove_dir_all(&d).ok();
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("review-decisions.jsonl"), "le vrai tri\n").unwrap();
        assert!(dataset(1, 3, &d.join("absent/..")).is_err());
        assert_eq!(std::fs::read_to_string(d.join("review-decisions.jsonl")).unwrap(), "le vrai tri\n");
        assert!(!d.join("absent").exists());
    }

    #[test]
    fn same_seed_gives_same_dataset() {
        let make = |name: &str| {
            let d = std::env::temp_dir().join(name);
            std::fs::remove_dir_all(&d).ok();
            dataset(7, 20, &d).unwrap();
            load(&d).unwrap().cases
        };
        assert_eq!(make("cs_banc_synth_a"), make("cs_banc_synth_b"));
    }
}
