//! Turn probabilities into decisions, following the guidance in the
//! [confidence](https://docs.typesafe.ai/confidence) docs: act automatically when the model is
//! sure, route to review when it is not, and keep the raw probabilities available for tuning.
//!
//! ```
//! use typesafeai_sdk_community::decision::{Bands, Decision, Gate, Outcome};
//! use typesafeai_sdk_community::{ChoiceAnswer, NoulAnswer};
//!
//! let spam = NoulAnswer::new(0.55);
//! assert!(spam.decide(0.5));
//! assert_eq!(spam.decide_with(Bands::new(0.3, 0.7)), Decision::Uncertain);
//!
//! let gate = Gate::new(0.85, 0.6); // accept at >= 0.85, review at >= 0.6, reject below
//! let tone = ChoiceAnswer::new("angry", 0.7, [("angry", 0.7), ("calm", 0.3)]);
//! assert_eq!(gate.evaluate(tone.confidence), Outcome::Review);
//! assert!((tone.margin() - 0.4).abs() < 1e-9);
//! ```
//!
//! Thresholds are yours to choose; evaluate them on your own data and consequences.

use crate::response::{ChoiceAnswer, NoulAnswer, ScoreAnswer};

/// A three-way reading of a yes/no probability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Decision {
    /// The probability is at or above the yes band.
    Yes,
    /// The probability is below the no band.
    No,
    /// The probability falls between the bands.
    Uncertain,
}

/// Probability bands for a yes/no decision: `no` below `no_below`, `yes` at or above
/// `yes_at`, uncertain in between.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bands {
    no_below: f64,
    yes_at: f64,
}

impl Bands {
    /// Bands with `no` below `no_below` and `yes` at or above `yes_at`. The values are clamped
    /// to 0..1 and ordered, so `Bands::new(0.7, 0.3)` is the same as `Bands::new(0.3, 0.7)`.
    pub fn new(no_below: f64, yes_at: f64) -> Self {
        let (a, b) = (no_below.clamp(0.0, 1.0), yes_at.clamp(0.0, 1.0));
        Bands { no_below: a.min(b), yes_at: a.max(b) }
    }

    /// Symmetric bands `width` on either side of 0.5, e.g. `0.2` gives `0.3..0.7`.
    pub fn around_half(width: f64) -> Self {
        Bands::new(0.5 - width, 0.5 + width)
    }

    /// Classify a probability.
    pub fn decide(&self, probability: f64) -> Decision {
        if probability >= self.yes_at {
            Decision::Yes
        } else if probability < self.no_below {
            Decision::No
        } else {
            Decision::Uncertain
        }
    }
}

impl NoulAnswer {
    /// `true` when the probability of yes is at or above `threshold`.
    pub fn decide(&self, threshold: f64) -> bool {
        self.noul >= threshold
    }

    /// A three-way decision using probability bands.
    pub fn decide_with(&self, bands: Bands) -> Decision {
        bands.decide(self.noul)
    }

    /// How far the probability is from 0.5, scaled to 0..1: `0` is a coin flip, `1` is certain
    /// either way.
    pub fn certainty(&self) -> f64 {
        ((self.noul - 0.5).abs() * 2.0).clamp(0.0, 1.0)
    }
}

/// What to do with an answer given its confidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Outcome {
    /// Confidence is high enough to act on automatically.
    Accept,
    /// Confidence is middling; send to a person or a slower model.
    Review,
    /// Confidence is too low to use.
    Reject,
}

/// Confidence thresholds for acting automatically, reviewing, or rejecting.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gate {
    accept_at: f64,
    review_at: f64,
}

impl Gate {
    /// Accept at or above `accept_at`, review at or above `review_at`, reject below. The values
    /// are clamped to 0..1 and ordered.
    pub fn new(accept_at: f64, review_at: f64) -> Self {
        let (a, b) = (accept_at.clamp(0.0, 1.0), review_at.clamp(0.0, 1.0));
        Gate { accept_at: a.max(b), review_at: a.min(b) }
    }

    /// Accept at or above `accept_at`, otherwise review; nothing is rejected.
    pub fn accept_or_review(accept_at: f64) -> Self {
        Gate::new(accept_at, 0.0)
    }

    /// Classify a confidence value.
    pub fn evaluate(&self, confidence: f64) -> Outcome {
        if confidence >= self.accept_at {
            Outcome::Accept
        } else if confidence >= self.review_at {
            Outcome::Review
        } else {
            Outcome::Reject
        }
    }
}

/// The gap between the largest and second-largest probability, from 0 to 1.
pub(crate) fn margin(probabilities: impl Iterator<Item = f64>) -> f64 {
    let (mut first, mut second) = (0.0f64, 0.0f64);
    for p in probabilities {
        if p > first {
            second = first;
            first = p;
        } else if p > second {
            second = p;
        }
    }
    (first - second).clamp(0.0, 1.0)
}

/// Shannon entropy in bits of a distribution (zero-probability entries are ignored).
pub(crate) fn entropy_bits(probabilities: impl Iterator<Item = f64>) -> f64 {
    probabilities.filter(|p| *p > 0.0).map(|p| -p * p.log2()).sum()
}

impl ChoiceAnswer {
    /// Labels and probabilities sorted from most to least likely.
    pub fn ranked(&self) -> Vec<(&str, f64)> {
        let mut ranked: Vec<(&str, f64)> = self.probabilities.iter().map(|(l, p)| (l.as_str(), *p)).collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        ranked
    }

    /// The `n` most likely labels.
    pub fn top(&self, n: usize) -> Vec<(&str, f64)> {
        let mut ranked = self.ranked();
        ranked.truncate(n);
        ranked
    }

    /// The probability of a label, or 0 if the response did not include it.
    pub fn probability(&self, label: &str) -> f64 {
        self.probabilities.get(label).copied().unwrap_or(0.0)
    }

    /// The gap between the most likely label and the runner-up, from 0 to 1. A small margin
    /// means two labels are competing even if `confidence` looks acceptable.
    pub fn margin(&self) -> f64 {
        margin(self.probabilities.values().copied())
    }

    /// Shannon entropy of the distribution in bits: 0 when one label has all the probability.
    pub fn entropy(&self) -> f64 {
        entropy_bits(self.probabilities.values().copied())
    }

    /// Entropy scaled to 0..1 by the number of labels, so distributions over different label
    /// counts are comparable. 0 is certain, 1 is uniform.
    pub fn normalized_entropy(&self) -> f64 {
        let n = self.probabilities.len();
        if n < 2 { 0.0 } else { (self.entropy() / (n as f64).log2()).clamp(0.0, 1.0) }
    }

    /// Classify this answer's confidence with a [`Gate`].
    pub fn gate(&self, gate: Gate) -> Outcome {
        gate.evaluate(self.confidence)
    }
}

impl ScoreAnswer {
    /// Levels and probabilities sorted from most to least likely.
    pub fn ranked(&self) -> Vec<(u32, f64)> {
        let mut ranked: Vec<(u32, f64)> = self.probabilities.iter().map(|(l, p)| (*l, *p)).collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
        ranked
    }

    /// The probability of a level, or 0 if the response did not include it.
    pub fn probability(&self, level: u32) -> f64 {
        self.probabilities.get(&level).copied().unwrap_or(0.0)
    }

    /// The probability that the score is at least `level`.
    pub fn probability_at_least(&self, level: u32) -> f64 {
        self.probabilities.iter().filter(|(l, _)| **l >= level).map(|(_, p)| *p).sum()
    }

    /// The probability that the score is at most `level`.
    pub fn probability_at_most(&self, level: u32) -> f64 {
        self.probabilities.iter().filter(|(l, _)| **l <= level).map(|(_, p)| *p).sum()
    }

    /// The highest level in the rubric, from the legend (or the probabilities if the legend is
    /// empty).
    pub fn max_level(&self) -> u32 {
        self.legend.keys().chain(self.probabilities.keys()).copied().max().unwrap_or(0)
    }

    /// The expected score scaled to 0..1 by the rubric's highest level, for combining scores
    /// from rubrics of different sizes.
    pub fn normalized(&self) -> f64 {
        let max = self.max_level();
        if max == 0 { 0.0 } else { (self.score / f64::from(max)).clamp(0.0, 1.0) }
    }

    /// Standard deviation of the level distribution around the expected score.
    pub fn std_dev(&self) -> f64 {
        self.probabilities.iter().map(|(l, p)| p * (f64::from(*l) - self.score).powi(2)).sum::<f64>().sqrt()
    }

    /// The gap between the most likely level and the runner-up, from 0 to 1.
    pub fn margin(&self) -> f64 {
        margin(self.probabilities.values().copied())
    }

    /// Classify this answer's confidence with a [`Gate`].
    pub fn gate(&self, gate: Gate) -> Outcome {
        gate.evaluate(self.confidence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn choice(probabilities: &[(&str, f64)]) -> ChoiceAnswer {
        let map: BTreeMap<String, f64> = probabilities.iter().map(|(l, p)| (l.to_string(), *p)).collect();
        let choice = map.iter().max_by(|a, b| a.1.total_cmp(b.1)).map(|(l, _)| l.clone()).unwrap_or_default();
        ChoiceAnswer { choice, confidence: 0.5, probabilities: map }
    }

    fn score(probabilities: &[(u32, f64)]) -> ScoreAnswer {
        let map: BTreeMap<u32, f64> = probabilities.iter().copied().collect();
        let expected = map.iter().map(|(l, p)| f64::from(*l) * p).sum();
        ScoreAnswer { score: expected, confidence: 0.5, legend: BTreeMap::new(), probabilities: map }
    }

    #[test]
    fn bands_and_decisions() {
        let bands = Bands::new(0.3, 0.7);
        assert_eq!(bands.decide(0.7), Decision::Yes);
        assert_eq!(bands.decide(0.69), Decision::Uncertain);
        assert_eq!(bands.decide(0.3), Decision::Uncertain);
        assert_eq!(bands.decide(0.29), Decision::No);
        assert_eq!(Bands::new(0.7, 0.3), bands);
        assert_eq!(Bands::around_half(0.2), bands);
        let noul = NoulAnswer { noul: 0.9 };
        assert!(noul.decide(0.9));
        assert!(!noul.decide(0.91));
        assert_eq!(noul.decide_with(bands), Decision::Yes);
        assert!((noul.certainty() - 0.8).abs() < 1e-9);
        assert_eq!(NoulAnswer { noul: 0.5 }.certainty(), 0.0);
    }

    #[test]
    fn gates() {
        let gate = Gate::new(0.85, 0.6);
        assert_eq!(gate.evaluate(0.85), Outcome::Accept);
        assert_eq!(gate.evaluate(0.6), Outcome::Review);
        assert_eq!(gate.evaluate(0.59), Outcome::Reject);
        assert_eq!(Gate::new(0.6, 0.85), gate);
        assert_eq!(Gate::accept_or_review(0.9).evaluate(0.0), Outcome::Review);
        assert_eq!(choice(&[("a", 1.0)]).gate(gate), Outcome::Reject); // confidence 0.5 in the fixture
    }

    #[test]
    fn choice_statistics() {
        let answer = choice(&[("a", 0.5), ("b", 0.3), ("c", 0.2)]);
        assert_eq!(answer.ranked(), vec![("a", 0.5), ("b", 0.3), ("c", 0.2)]);
        assert_eq!(answer.top(2), vec![("a", 0.5), ("b", 0.3)]);
        assert_eq!(answer.probability("b"), 0.3);
        assert_eq!(answer.probability("zzz"), 0.0);
        assert!((answer.margin() - 0.2).abs() < 1e-9);
        assert!(answer.entropy() > 1.4 && answer.entropy() < 1.5);
        let uniform = choice(&[("a", 0.5), ("b", 0.5)]);
        assert!((uniform.normalized_entropy() - 1.0).abs() < 1e-9);
        assert_eq!(uniform.margin(), 0.0);
        assert_eq!(choice(&[("a", 1.0)]).normalized_entropy(), 0.0);
        assert_eq!(choice(&[("a", 1.0)]).entropy(), 0.0);
    }

    #[test]
    fn score_statistics() {
        let answer = score(&[(0, 0.1), (1, 0.1), (2, 0.8)]);
        assert!((answer.score - 1.7).abs() < 1e-9);
        assert_eq!(answer.ranked()[0], (2, 0.8));
        assert!((answer.probability_at_least(1) - 0.9).abs() < 1e-9);
        assert!((answer.probability_at_most(1) - 0.2).abs() < 1e-9);
        assert_eq!(answer.max_level(), 2);
        assert!((answer.normalized() - 0.85).abs() < 1e-9);
        assert!((answer.std_dev() - 0.640_312_4).abs() < 1e-6);
        assert!((answer.margin() - 0.7).abs() < 1e-9);
        assert_eq!(score(&[(0, 1.0)]).normalized(), 0.0);
        assert_eq!(score(&[]).max_level(), 0);
    }
}
