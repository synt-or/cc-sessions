//! Modèle factice à précision connue : il sert à tester le banc et ses scores.
use crate::adapter::{validate, Classifier, Prediction};
use crate::categories::CATEGORIES;
use crate::dataset::Case;
use crate::synth::Rng;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};

pub struct Mock {
    accuracy: f64,
    seed: u64,
    rng: RefCell<Rng>,
    truths: HashMap<String, String>,
}

impl Mock {
    pub fn new(accuracy: f64, seed: u64, cases: &[Case]) -> Mock {
        Mock {
            accuracy,
            seed,
            rng: RefCell::new(Rng::new(seed)),
            truths: cases.iter().map(|c| (c.text.clone(), c.truth.clone())).collect(),
        }
    }
}

impl Classifier for Mock {
    fn name(&self) -> &str {
        "mock"
    }

    fn target(&self) -> String {
        "mock".into()
    }

    fn preamble(&self) -> String {
        "aucun : le modèle factice n'envoie rien".into()
    }

    fn signature(&self) -> String {
        format!("mock {} {}", self.accuracy, self.seed)
    }

    fn predict(&self, text: &str) -> Result<Prediction, String> {
        let truth = self.truths.get(text).ok_or("cas inconnu du modèle factice")?;
        let mut rng = self.rng.borrow_mut();
        let answer = if rng.unit() < self.accuracy {
            truth.clone()
        } else {
            let others: Vec<&str> = CATEGORIES
                .iter()
                .map(|(k, _)| *k)
                .filter(|k| *k != truth)
                .collect();
            others[rng.below(others.len())].to_string()
        };
        let probabilities: BTreeMap<String, f64> = CATEGORIES
            .iter()
            .map(|(k, _)| (k.to_string(), if *k == answer { 0.6 } else { 0.1 }))
            .collect();
        validate(probabilities, 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_reaches_requested_accuracy() {
        let case = Case {
            session_id: "s".into(),
            snapshot_hash: "h".into(),
            text: "t".into(),
            truth: "hold".into(),
            has_prior_flag: false,
            prior_status: None,
        };
        let mock = Mock::new(0.7, 3, &[case]);
        let right = (0..2000)
            .filter(|_| mock.predict("t").unwrap().top() == Some("hold"))
            .count();
        let share = right as f64 / 2000.0;
        assert!((0.66..0.74).contains(&share), "{share}");
    }
}
