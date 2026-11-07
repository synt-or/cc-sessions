//! Scores d'un modèle contre le tri de l'utilisateur.
use crate::adapter::Prediction;
use crate::categories::CATEGORIES;
use crate::synth::Rng;
use std::collections::BTreeMap;

const BOOTSTRAP_DRAWS: usize = 2000;
const MIN_PROBABILITY: f64 = 1e-12;
const CALIBRATION_BINS: usize = 5;

/// Un cas prédit : la catégorie du tri, et la réponse du modèle ou son échec.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub truth: String,
    pub has_prior_flag: bool,
    pub prior_status: Option<String>,
    pub result: Result<Prediction, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CalibrationBin {
    pub low: f64,
    pub high: f64,
    pub n: usize,
    pub mean_confidence: f64,
    pub accuracy: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub n: usize,
    pub failures: usize,
    pub accuracy: f64,
    pub accuracy_ci95: (f64, f64),
    pub macro_f1: f64,
    pub top2: f64,
    /// Sur les réponses valides seulement.
    pub log_loss: f64,
    pub brier: f64,
    pub kappa: f64,
    pub recall_by_category: BTreeMap<String, f64>,
    /// `confusion[vérité][prédiction]`, avec `(échec)` pour un modèle sans réponse.
    pub confusion: BTreeMap<String, BTreeMap<String, usize>>,
    pub calibration: Vec<CalibrationBin>,
    pub latency_p50_ms: u64,
    pub latency_p95_ms: u64,
    /// Toujours répondre la catégorie majoritaire.
    pub baseline_accuracy: f64,
    /// Reprendre le statut du bandeau, pour les cas qui en ont un.
    pub copy_flag_accuracy: Option<f64>,
}

pub const FAILURE: &str = "(échec)";
pub const TIE: &str = "(égalité)";

fn predicted(o: &Outcome) -> &str {
    match &o.result {
        Ok(p) => p.top().unwrap_or(TIE),
        Err(_) => FAILURE,
    }
}

fn share(count: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        count as f64 / total as f64
    }
}

fn percentile(sorted: &[u64], q: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

fn accuracy_of(outcomes: &[Outcome]) -> f64 {
    share(outcomes.iter().filter(|o| predicted(o) == o.truth).count(), outcomes.len())
}

fn bootstrap_ci(hits: &[bool], seed: u64) -> (f64, f64) {
    if hits.is_empty() {
        return (0.0, 0.0);
    }
    let mut rng = Rng::new(seed);
    let mut draws: Vec<f64> = (0..BOOTSTRAP_DRAWS)
        .map(|_| {
            let count = (0..hits.len()).filter(|_| hits[rng.below(hits.len())]).count();
            share(count, hits.len())
        })
        .collect();
    draws.sort_by(f64::total_cmp);
    (draws[(BOOTSTRAP_DRAWS as f64 * 0.025) as usize], draws[(BOOTSTRAP_DRAWS as f64 * 0.975) as usize])
}

pub fn score(outcomes: &[Outcome], seed: u64) -> Report {
    let n = outcomes.len();
    let keys: Vec<&str> = CATEGORIES.iter().map(|(k, _)| *k).collect();
    let valid: Vec<(&Outcome, &Prediction)> = outcomes
        .iter()
        .filter_map(|o| o.result.as_ref().ok().map(|p| (o, p)))
        .collect();

    let mut confusion: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for o in outcomes {
        *confusion
            .entry(o.truth.clone())
            .or_default()
            .entry(predicted(o).to_string())
            .or_default() += 1;
    }
    let cell = |t: &str, p: &str| confusion.get(t).and_then(|r| r.get(p)).copied().unwrap_or(0);
    let truth_total = |k: &str| outcomes.iter().filter(|o| o.truth == k).count();
    let pred_total = |k: &str| outcomes.iter().filter(|o| predicted(o) == k).count();

    // Sur les catégories présentes seulement : une catégorie sans aucun cas n'a pas de
    // rappel, et ne doit pas tirer le score vers le bas dans un petit sous-groupe.
    let mut recall_by_category = BTreeMap::new();
    let mut f1_sum = 0.0;
    let mut present = 0;
    for k in &keys {
        if truth_total(k) == 0 && pred_total(k) == 0 {
            continue;
        }
        present += 1;
        let tp = cell(k, k);
        let recall = share(tp, truth_total(k));
        let precision = share(tp, pred_total(k));
        if truth_total(k) > 0 {
            recall_by_category.insert(k.to_string(), recall);
        }
        if precision + recall > 0.0 {
            f1_sum += 2.0 * precision * recall / (precision + recall);
        }
    }

    let hits: Vec<bool> = outcomes.iter().map(|o| predicted(o) == o.truth).collect();
    let accuracy = accuracy_of(outcomes);

    let top2_hits = valid.iter().filter(|(o, p)| p.in_top2(&o.truth)).count();

    let log_loss = if valid.is_empty() {
        0.0
    } else {
        valid
            .iter()
            .map(|(o, p)| {
                -p.probabilities
                    .get(&o.truth)
                    .copied()
                    .unwrap_or(0.0)
                    .max(MIN_PROBABILITY)
                    .ln()
            })
            .sum::<f64>()
            / valid.len() as f64
    };
    let brier = if valid.is_empty() {
        0.0
    } else {
        valid
            .iter()
            .map(|(o, p)| {
                keys.iter()
                    .map(|k| {
                        let target = if *k == o.truth { 1.0 } else { 0.0 };
                        (p.probabilities.get(*k).copied().unwrap_or(0.0) - target).powi(2)
                    })
                    .sum::<f64>()
            })
            .sum::<f64>()
            / valid.len() as f64
    };

    // Kappa de Cohen : l'accord au-delà du hasard. Un échec est une réponse à part.
    let expected: f64 = keys
        .iter()
        .map(|k| share(truth_total(k), n) * share(pred_total(k), n))
        .sum();
    let kappa = if (1.0 - expected).abs() < f64::EPSILON {
        // Une seule catégorie partout : rien à départager au-delà du hasard.
        if accuracy >= 1.0 { 1.0 } else { 0.0 }
    } else {
        (accuracy - expected) / (1.0 - expected)
    };

    let calibration = (0..CALIBRATION_BINS)
        .filter_map(|bin| {
            let low = bin as f64 / CALIBRATION_BINS as f64;
            let high = (bin + 1) as f64 / CALIBRATION_BINS as f64;
            let members: Vec<(f64, bool)> = valid
                .iter()
                .map(|(o, p)| {
                    let confidence = p.probabilities.values().copied().fold(0.0, f64::max);
                    (confidence, p.top() == Some(o.truth.as_str()))
                })
                .filter(|(c, _)| {
                    *c >= low && (*c < high || bin == CALIBRATION_BINS - 1)
                })
                .collect();
            (!members.is_empty()).then(|| CalibrationBin {
                low,
                high,
                n: members.len(),
                mean_confidence: members.iter().map(|m| m.0).sum::<f64>() / members.len() as f64,
                accuracy: share(members.iter().filter(|m| m.1).count(), members.len()),
            })
        })
        .collect();

    let mut latencies: Vec<u64> = valid.iter().map(|(_, p)| p.latency_ms).collect();
    latencies.sort_unstable();

    Report {
        n,
        failures: n - valid.len(),
        accuracy,
        accuracy_ci95: bootstrap_ci(&hits, seed),
        macro_f1: if present == 0 { 0.0 } else { f1_sum / present as f64 },
        top2: share(top2_hits, n),
        log_loss,
        brier,
        kappa,
        recall_by_category,
        confusion,
        calibration,
        latency_p50_ms: percentile(&latencies, 0.5),
        latency_p95_ms: percentile(&latencies, 0.95),
        baseline_accuracy: share(keys.iter().map(|k| truth_total(k)).max().unwrap_or(0), n),
        copy_flag_accuracy: copy_flag_accuracy(outcomes),
    }
}

fn copy_flag_accuracy(outcomes: &[Outcome]) -> Option<f64> {
    let flagged: Vec<&Outcome> = outcomes.iter().filter(|o| o.prior_status.is_some()).collect();
    if flagged.is_empty() {
        return None;
    }
    let same = flagged
        .iter()
        .filter(|o| o.prior_status.as_deref() == Some(o.truth.as_str()))
        .count();
    Some(share(same, flagged.len()))
}

/// L'avantage d'un modèle sur la référence naïve, mesuré cas par cas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Advantage {
    pub difference: f64,
    pub low: f64,
    pub high: f64,
    /// Le niveau de confiance, après correction pour le nombre de modèles comparés.
    pub confidence: f64,
}

impl Advantage {
    pub fn beats_baseline(&self) -> bool {
        self.low > 0.0
    }
}

/// Bootstrap apparié de (succès du modèle − succès de la référence), avec correction de
/// Bonferroni : plus on compare de modèles, plus un écart peut être dû à la chance.
pub fn advantage(outcomes: &[Outcome], models_compared: usize, seed: u64) -> Option<Advantage> {
    if outcomes.is_empty() || models_compared == 0 {
        return None;
    }
    let majority = CATEGORIES
        .iter()
        .map(|(k, _)| *k)
        .max_by_key(|k| outcomes.iter().filter(|o| o.truth == *k).count())?;
    let gains: Vec<f64> = outcomes
        .iter()
        .map(|o| f64::from(predicted(o) == o.truth) - f64::from(o.truth == majority))
        .collect();
    let mean = |values: &mut dyn Iterator<Item = f64>| values.sum::<f64>() / gains.len() as f64;
    let mut rng = Rng::new(seed);
    let mut draws: Vec<f64> = (0..BOOTSTRAP_DRAWS)
        .map(|_| mean(&mut (0..gains.len()).map(|_| gains[rng.below(gains.len())])))
        .collect();
    draws.sort_by(f64::total_cmp);
    let alpha = 0.05 / models_compared as f64;
    let at = |q: f64| draws[((BOOTSTRAP_DRAWS - 1) as f64 * q).round() as usize];
    Some(Advantage {
        difference: mean(&mut gains.iter().copied()),
        low: at(alpha / 2.0),
        high: at(1.0 - alpha / 2.0),
        confidence: 1.0 - alpha,
    })
}

/// Le bandeau donne un indice : les scores sont aussi donnés sans ces sessions.
pub fn split_by_flag(outcomes: &[Outcome]) -> (Vec<Outcome>, Vec<Outcome>) {
    outcomes.iter().cloned().partition(|o| o.has_prior_flag)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::validate;

    fn dist(top: &str, p: f64) -> BTreeMap<String, f64> {
        let rest = (1.0 - p) / 4.0;
        CATEGORIES
            .iter()
            .map(|(k, _)| (k.to_string(), if *k == top { p } else { rest }))
            .collect()
    }

    fn outcome(truth: &str, top: &str, p: f64) -> Outcome {
        Outcome {
            truth: truth.into(),
            has_prior_flag: false,
            prior_status: None,
            result: validate(dist(top, p), 10),
        }
    }

    fn failed(truth: &str) -> Outcome {
        Outcome {
            truth: truth.into(),
            has_prior_flag: false,
            prior_status: None,
            result: Err("panne".into()),
        }
    }

    #[test]
    fn perfect_model_scores_one() {
        let o: Vec<_> = CATEGORIES.iter().map(|(k, _)| outcome(k, k, 1.0)).collect();
        let r = score(&o, 1);
        assert_eq!(r.accuracy, 1.0);
        assert!((r.macro_f1 - 1.0).abs() < 1e-9);
        assert!((r.kappa - 1.0).abs() < 1e-9);
        assert!(r.log_loss.abs() < 1e-9);
        assert!(r.brier.abs() < 1e-9);
    }

    #[test]
    fn uniform_model_log_loss_is_ln5() {
        let o = vec![outcome("hold", "hold", 0.2), outcome("done", "hold", 0.2)];
        let r = score(&o, 1);
        assert!((r.log_loss - 5f64.ln()).abs() < 1e-6, "{}", r.log_loss);
    }

    #[test]
    fn hand_computed_confusion_and_f1() {
        // hold : 2 justes, 1 prédit done. done : 1 juste, 1 prédit hold. active : 1 prédit hold.
        let o = vec![
            outcome("hold", "hold", 0.9),
            outcome("hold", "hold", 0.9),
            outcome("hold", "done", 0.9),
            outcome("done", "done", 0.9),
            outcome("done", "hold", 0.9),
            outcome("active", "hold", 0.9),
        ];
        let r = score(&o, 1);
        assert_eq!(r.confusion["hold"]["hold"], 2);
        assert_eq!(r.confusion["hold"]["done"], 1);
        assert_eq!(r.confusion["active"]["hold"], 1);
        assert!((r.accuracy - 0.5).abs() < 1e-9);
        // hold : précision 2/4, rappel 2/3, F1 = 4/7. done : 1/2 et 1/2, F1 = 1/2. active : 0.
        // Les deux catégories sans aucun cas ne comptent pas.
        let expected = (4.0 / 7.0 + 0.5) / 3.0;
        assert!((r.macro_f1 - expected).abs() < 1e-9, "{}", r.macro_f1);
        assert!((r.recall_by_category["hold"] - 2.0 / 3.0).abs() < 1e-9);
        assert!((r.baseline_accuracy - 0.5).abs() < 1e-9);
    }

    #[test]
    fn failures_count_as_errors_for_accuracy_only() {
        let o = vec![
            outcome("hold", "hold", 1.0),
            outcome("hold", "hold", 1.0),
            outcome("hold", "hold", 1.0),
            failed("hold"),
        ];
        let r = score(&o, 1);
        assert_eq!(r.failures, 1);
        assert!((r.accuracy - 0.75).abs() < 1e-9);
        assert!(r.log_loss.abs() < 1e-9, "échec exclu du log loss");
        assert_eq!(r.confusion["hold"][FAILURE], 1);
    }

    #[test]
    fn majority_model_equals_baseline_and_kappa_zero() {
        let o = vec![
            outcome("hold", "hold", 0.9),
            outcome("hold", "hold", 0.9),
            outcome("hold", "hold", 0.9),
            outcome("done", "hold", 0.9),
        ];
        let r = score(&o, 1);
        assert!((r.accuracy - r.baseline_accuracy).abs() < 1e-9);
        assert!(r.kappa.abs() < 1e-9, "{}", r.kappa);
    }

    #[test]
    fn ci_is_wide_on_small_sample() {
        let o: Vec<_> = (0..83)
            .map(|i| if i < 58 { outcome("hold", "hold", 0.9) } else { outcome("hold", "done", 0.9) })
            .collect();
        let r = score(&o, 1);
        assert!(r.accuracy_ci95.1 - r.accuracy_ci95.0 > 0.15, "{:?}", r.accuracy_ci95);
        assert!(r.accuracy_ci95.0 < r.accuracy && r.accuracy < r.accuracy_ci95.1);
    }

    #[test]
    fn calibration_compares_confidence_and_accuracy() {
        let o = vec![outcome("hold", "hold", 0.9), outcome("hold", "done", 0.9)];
        let r = score(&o, 1);
        assert_eq!(r.calibration.len(), 1);
        assert_eq!(r.calibration[0].n, 2);
        assert!((r.calibration[0].mean_confidence - 0.9).abs() < 1e-9);
        assert!((r.calibration[0].accuracy - 0.5).abs() < 1e-9);
    }

    #[test]
    fn absent_category_has_no_recall() {
        let r = score(&[outcome("hold", "hold", 0.9), outcome("hold", "hold", 0.9)], 1);
        assert!((r.macro_f1 - 1.0).abs() < 1e-9, "{}", r.macro_f1);
        assert!(!r.recall_by_category.contains_key("done"));
    }

    #[test]
    fn tied_answer_is_not_a_hit_and_stays_coherent() {
        let r = score(&[outcome("ready_to_burn", "hold", 0.2)], 1);
        assert_eq!(r.accuracy, 0.0);
        assert_eq!(r.top2, 0.0);
        assert_eq!(r.failures, 0);
        assert_eq!(r.confusion["ready_to_burn"][TIE], 1);
    }

    #[test]
    fn copying_the_banner_is_measured() {
        let mut a = outcome("hold", "hold", 0.9);
        a.prior_status = Some("hold".into());
        let mut b = outcome("done", "hold", 0.9);
        b.prior_status = Some("hold".into());
        let r = score(&[a, b, outcome("done", "done", 0.9)], 1);
        assert_eq!(r.copy_flag_accuracy, Some(0.5));
        assert_eq!(score(&[outcome("done", "done", 0.9)], 1).copy_flag_accuracy, None);
    }

    #[test]
    fn clear_winner_beats_the_baseline() {
        let cases: Vec<Outcome> = (0..82)
            .map(|i| {
                let truth = if i % 2 == 0 { "hold" } else { "done" };
                outcome(truth, truth, 0.9)
            })
            .collect();
        let a = advantage(&cases, 6, 1).unwrap();
        assert!(a.beats_baseline(), "{a:?}");
        assert!((a.difference - 0.5).abs() < 1e-9);
    }

    #[test]
    fn best_of_six_chance_models_rarely_beats_the_baseline() {
        // Six modèles sans savoir, aussi justes que la référence (40 % de majorité) : le
        // faux verdict était à 18 % avec l'intervalle simple. Il doit passer sous 10 %.
        let mut rng = Rng::new(11);
        let truths: Vec<&str> = (0..82)
            .map(|i| match i * 10 / 82 {
                0..=3 => "hold",
                4..=6 => "done",
                _ => "active",
            })
            .collect();
        let trials = 60;
        let mut wins = 0;
        for trial in 0..trials {
            let any = (0..6).any(|model| {
                let cases: Vec<Outcome> = truths
                    .iter()
                    .map(|t| {
                        let right = rng.unit() < 0.4;
                        let wrong = if *t == "hold" { "done" } else { "hold" };
                        outcome(t, if right { t } else { wrong }, 0.9)
                    })
                    .collect();
                advantage(&cases, 6, (trial * 10 + model) as u64 + 1).unwrap().beats_baseline()
            });
            wins += usize::from(any);
        }
        assert!(wins * 10 < trials, "{wins} sur {trials}");
    }

    #[test]
    fn empty_input_does_not_panic() {
        let r = score(&[], 1);
        assert_eq!(r.n, 0);
        assert_eq!(r.accuracy, 0.0);
    }

    #[test]
    fn split_separates_sessions_with_a_prior_flag() {
        let mut a = outcome("hold", "hold", 0.9);
        a.has_prior_flag = true;
        let (with, without) = split_by_flag(&[a, outcome("done", "done", 0.9)]);
        assert_eq!((with.len(), without.len()), (1, 1));
    }
}
