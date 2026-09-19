//! Composite scoring: combine several answers with weights that live in code, following the
//! [composite scoring](https://docs.typesafe.ai/patterns/composite-scoring) pattern. Ask each
//! dimension once, then reweight, re-rank, or re-threshold without re-running inference.
//!
//! With the `derive` feature, annotate a [`Questions`](crate::typed::Questions) struct:
//!
//! ```
//! use typesafeai_sdk_community::composite::{Composite as _, Weights};
//! use typesafeai_sdk_community::{Composite, NoulAnswer, Questions, ScoreAnswer, TypedChoice, ChoiceLabels};
//!
//! #[derive(Clone, Copy, Debug, PartialEq, Eq, ChoiceLabels)]
//! enum Tone { Angry, Calm }
//!
//! #[derive(Questions, Composite)]
//! struct SpamRisk {
//!     #[noul("Does the message ask for credentials?")]
//!     #[weight(0.45)]
//!     credentials: NoulAnswer,
//!     #[noul("Does the sender identity look spoofed?")]
//!     #[weight(0.30)]
//!     spoofed: NoulAnswer,
//!     #[score("How unexpected is the reward?", levels = ["expected", "surprising", "too good to be true"])]
//!     #[weight(0.25)]
//!     reward: ScoreAnswer,
//!     #[choice("Tone?")]
//!     #[weight(0.10, label = Tone::Angry, invert)]
//!     tone: TypedChoice<Tone>,
//! }
//!
//! # let risk = SpamRisk {
//! #     credentials: NoulAnswer { noul: 0.9 },
//! #     spoofed: NoulAnswer { noul: 0.2 },
//! #     reward: ScoreAnswer { score: 2.0, confidence: 1.0, legend: [(0, "a".into()), (1, "b".into()), (2, "c".into())].into(), probabilities: [(2, 1.0)].into() },
//! #     tone: TypedChoice { choice: Tone::Calm, confidence: 1.0, probabilities: vec![(Tone::Calm, 1.0), (Tone::Angry, 0.0)] },
//! # };
//! let score = risk.composite();                    // 0..1 with the declared weights
//! let tuned = Weights::from([("credentials", 0.6), ("spoofed", 0.4)]);
//! let score = risk.composite_with(&tuned);         // only the named signals count
//! for part in risk.breakdown() {
//!     println!("{:<12} signal={:?} weight={:.2} contributes={:.3}", part.name, part.signal, part.weight, part.contribution);
//! }
//! ```
//!
//! Every signal is a number from 0 to 1: a noul's probability, a score's expected value
//! divided by its highest level, a `bool` as 0 or 1, an `f64` as-is, or the probability of one
//! label of a choice (`label = ...`). `invert` uses `1 - signal`. Missing signals (`None` in
//! an `Option` field) are left out and the remaining weights are renormalized. [`Weights`] is
//! serializable, so weights can come from configuration.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::response::{Answer, ChoiceAnswer, NoulAnswer, ScoreAnswer};
use crate::typed::{ChoiceLabels, ScoreLevels, TypedChoice, TypedScore};

/// A value that can be read as a number from 0 to 1.
pub trait Signal {
    /// The signal, or `None` when it is absent.
    fn signal(&self) -> Option<f64>;
}

impl Signal for NoulAnswer {
    fn signal(&self) -> Option<f64> {
        Some(self.noul)
    }
}

impl Signal for bool {
    fn signal(&self) -> Option<f64> {
        Some(if *self { 1.0 } else { 0.0 })
    }
}

/// Used as-is; callers are responsible for keeping it in 0..1.
impl Signal for f64 {
    fn signal(&self) -> Option<f64> {
        Some(*self)
    }
}

impl Signal for ScoreAnswer {
    fn signal(&self) -> Option<f64> {
        Some(self.normalized())
    }
}

impl<T: ScoreLevels> Signal for TypedScore<T> {
    fn signal(&self) -> Option<f64> {
        Some(self.normalized())
    }
}

/// A noul's probability or a score's normalized value; a choice has no single signal (use
/// `label = ...`).
impl Signal for Answer {
    fn signal(&self) -> Option<f64> {
        match self {
            Answer::Noul(noul) => noul.signal(),
            Answer::Score(score) => score.signal(),
            _ => None,
        }
    }
}

impl<T: Signal> Signal for Option<T> {
    fn signal(&self) -> Option<f64> {
        self.as_ref().and_then(Signal::signal)
    }
}

/// A choice answer read as the probability of one label.
pub trait LabelSignal<L> {
    /// The probability of `label`, or `None` when the answer is absent.
    fn label_signal(&self, label: L) -> Option<f64>;
}

impl<L: AsRef<str>> LabelSignal<L> for ChoiceAnswer {
    fn label_signal(&self, label: L) -> Option<f64> {
        Some(self.probability(label.as_ref()))
    }
}

impl<T: ChoiceLabels> LabelSignal<T> for TypedChoice<T> {
    fn label_signal(&self, label: T) -> Option<f64> {
        Some(self.probability(label))
    }
}

impl<L: AsRef<str>> LabelSignal<L> for Answer {
    fn label_signal(&self, label: L) -> Option<f64> {
        self.as_choice().and_then(|choice| choice.label_signal(label))
    }
}

impl<L, T: LabelSignal<L>> LabelSignal<L> for Option<T> {
    fn label_signal(&self, label: L) -> Option<f64> {
        self.as_ref().and_then(|inner| inner.label_signal(label))
    }
}

/// Weights keyed by signal name. Serializable, so they can be loaded from configuration and
/// changed without touching the questions.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Weights(BTreeMap<String, f64>);

impl Weights {
    /// No weights.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a signal's weight.
    pub fn set(mut self, name: impl Into<String>, weight: f64) -> Self {
        self.0.insert(name.into(), weight);
        self
    }

    /// A signal's weight, or `None` if it has none.
    pub fn get(&self, name: &str) -> Option<f64> {
        self.0.get(name).copied()
    }

    /// Iterate over names and weights.
    pub fn iter(&self) -> impl Iterator<Item = (&str, f64)> {
        self.0.iter().map(|(name, weight)| (name.as_str(), *weight))
    }

    /// The sum of all weights.
    pub fn total(&self) -> f64 {
        self.0.values().sum()
    }
}

impl<N: Into<String>, const M: usize> From<[(N, f64); M]> for Weights {
    fn from(entries: [(N, f64); M]) -> Self {
        Weights(entries.into_iter().map(|(name, weight)| (name.into(), weight)).collect())
    }
}

impl FromIterator<(String, f64)> for Weights {
    fn from_iter<I: IntoIterator<Item = (String, f64)>>(iter: I) -> Self {
        Weights(iter.into_iter().collect())
    }
}

/// One signal's part in a composite score.
#[derive(Clone, Debug, PartialEq)]
pub struct Contribution {
    /// The signal name (the field name unless renamed).
    pub name: &'static str,
    /// The signal value from 0 to 1, after any `invert`, or `None` when absent.
    pub signal: Option<f64>,
    /// The weight applied, after renormalization over present signals.
    pub weight: f64,
    /// `signal * weight`, or 0 when absent.
    pub contribution: f64,
}

/// A set of weighted signals combined into one score from 0 to 1.
///
/// Implement with `#[derive(Composite)]`; only [`signals`](Self::signals) and
/// [`default_weights`](Self::default_weights) are required.
pub trait Composite {
    /// The weights declared with `#[weight(...)]`.
    fn default_weights() -> Weights;

    /// Every declared signal by name, with its current value.
    fn signals(&self) -> Vec<(&'static str, Option<f64>)>;

    /// The weighted average of the present signals using the declared weights.
    fn composite(&self) -> f64 {
        self.composite_with(&Self::default_weights())
    }

    /// The weighted average of the present signals using `weights`; signals without a weight
    /// (or with weight 0) are ignored, and weights are renormalized over the signals that are
    /// present.
    fn composite_with(&self, weights: &Weights) -> f64 {
        self.breakdown_with(weights).iter().map(|part| part.contribution).sum()
    }

    /// Each signal's value, effective weight, and contribution under the declared weights.
    fn breakdown(&self) -> Vec<Contribution> {
        self.breakdown_with(&Self::default_weights())
    }

    /// Each signal's value, effective weight, and contribution under `weights`.
    fn breakdown_with(&self, weights: &Weights) -> Vec<Contribution> {
        let signals = self.signals();
        let total: f64 = signals
            .iter()
            .filter(|(_, signal)| signal.is_some())
            .map(|(name, _)| weights.get(name).unwrap_or(0.0).max(0.0))
            .sum();
        signals
            .into_iter()
            .map(|(name, signal)| {
                let raw = weights.get(name).unwrap_or(0.0).max(0.0);
                let weight = match signal {
                    Some(_) if total > 0.0 => raw / total,
                    _ => 0.0,
                };
                let contribution = signal.map_or(0.0, |value| value.clamp(0.0, 1.0) * weight);
                Contribution { name, signal, weight, contribution }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Risk {
        a: NoulAnswer,
        b: Option<NoulAnswer>,
        c: bool,
    }

    impl Composite for Risk {
        fn default_weights() -> Weights {
            Weights::from([("a", 0.5), ("b", 0.3), ("c", 0.2)])
        }
        fn signals(&self) -> Vec<(&'static str, Option<f64>)> {
            vec![("a", self.a.signal()), ("b", self.b.signal()), ("c", self.c.signal())]
        }
    }

    #[test]
    fn weighted_average_with_renormalization() {
        let risk = Risk { a: NoulAnswer { noul: 0.8 }, b: Some(NoulAnswer { noul: 0.5 }), c: true };
        assert!((risk.composite() - (0.8 * 0.5 + 0.5 * 0.3 + 1.0 * 0.2)).abs() < 1e-9);

        let missing = Risk { a: NoulAnswer { noul: 0.8 }, b: None, c: false };
        // b is absent: a and c share the weight 0.5 : 0.2.
        assert!((missing.composite() - 0.8 * (0.5 / 0.7)).abs() < 1e-9);
        let parts = missing.breakdown();
        assert_eq!(parts[1], Contribution { name: "b", signal: None, weight: 0.0, contribution: 0.0 });
        assert!((parts[0].weight - 0.5 / 0.7).abs() < 1e-9);

        let only_a = risk.composite_with(&Weights::from([("a", 2.0)]));
        assert!((only_a - 0.8).abs() < 1e-9);
        assert_eq!(risk.composite_with(&Weights::new()), 0.0);
    }

    #[test]
    fn signals_and_weights() {
        assert_eq!(true.signal(), Some(1.0));
        assert_eq!(0.25f64.signal(), Some(0.25));
        assert_eq!(None::<bool>.signal(), None);
        let score =
            ScoreAnswer { score: 1.0, confidence: 1.0, legend: BTreeMap::new(), probabilities: [(2, 0.0)].into() };
        assert_eq!(score.signal(), Some(0.5));
        let choice =
            ChoiceAnswer { choice: "a".into(), confidence: 1.0, probabilities: [("a".to_string(), 0.7)].into() };
        assert_eq!(choice.label_signal("a"), Some(0.7));
        assert_eq!(Some(choice).label_signal("zzz"), Some(0.0));
        assert_eq!(Answer::Noul(NoulAnswer { noul: 0.3 }).signal(), Some(0.3));

        let weights = Weights::new().set("x", 1.5).set("y", 0.5);
        assert_eq!(weights.total(), 2.0);
        assert_eq!(weights.get("x"), Some(1.5));
        let json = serde_json::to_string(&weights).unwrap();
        assert_eq!(json, r#"{"x":1.5,"y":0.5}"#);
        assert_eq!(serde_json::from_str::<Weights>(&json).unwrap(), weights);
    }
}
