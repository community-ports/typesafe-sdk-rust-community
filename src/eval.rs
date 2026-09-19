//! Evaluate questions and thresholds on your own labeled data.
//!
//! The docs say to validate a model's judgments in the target domain and to choose thresholds
//! on your data and consequences. This module runs a question set over labeled examples and
//! reports how well the answers line up: for yes/no questions, Brier score, log loss, AUC, a
//! calibration table, and the best thresholds; for choices, accuracy, per-label precision and
//! recall, a confusion matrix, and how much can be auto-accepted at a given accuracy; for
//! scores, MAE, exact and within-one agreement.
//!
//! ```no_run
//! use typesafeai_sdk_community::eval::Example;
//! use typesafeai_sdk_community::{NoulAnswer, Questions, TypeSafeClient, json};
//!
//! #[derive(Debug, Questions)]
//! struct Spam {
//!     #[noul("Is `message` unsolicited advertising?")]
//!     spam: NoulAnswer,
//! }
//!
//! # async fn run(client: TypeSafeClient, messages: Vec<(String, bool)>) -> typesafeai_sdk_community::Result<()> {
//! let examples = messages.into_iter().map(|(text, is_spam)| Example::new(json!({"message": text}), is_spam));
//! let run = client.evaluate::<Spam, _>(examples).concurrency(8).run().await;
//! let report = run.binary(|spam| spam.spam.noul);
//! println!("{report}");
//! println!("use threshold {:.2} for the best F1 ({:.3})", report.best_f1.threshold, report.best_f1.f1);
//! # Ok(()) }
//! ```
//!
//! The metric functions ([`binary`], [`choice`], [`score`]) also work on any predictions you
//! already have, with no client involved.

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;
use serde_json::Value;

use crate::batch::Batch;
use crate::client::TypeSafeClient;
use crate::decision::Gate;
use crate::error::Error;
use crate::response::Usage;
use crate::typed::{Answered, ChoiceLabels, Questions, ScoreLevels};

// --- Examples and runs ----------------------------------------------------------------------

/// A labeled example: the state to send and the expected answer.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Example<L> {
    /// The state, as for any request.
    pub state: Value,
    /// The expected answer.
    pub label: L,
}

impl<L> Example<L> {
    /// A labeled example.
    pub fn new(state: impl Into<Value>, label: L) -> Self {
        Example { state: state.into(), label }
    }
}

impl<S: Into<Value>, L> From<(S, L)> for Example<L> {
    fn from((state, label): (S, L)) -> Self {
        Example::new(state, label)
    }
}

/// An evaluation under construction; see [`TypeSafeClient::evaluate`].
#[must_use = "an evaluation does nothing until it is run"]
pub struct Evaluate<'a, T, L> {
    client: &'a TypeSafeClient,
    examples: Vec<Example<L>>,
    concurrency: usize,
    model: Option<String>,
    _answers: std::marker::PhantomData<fn() -> T>,
}

impl<'a, T: Questions, L> Evaluate<'a, T, L> {
    /// Maximum requests in flight at once. Default 4.
    pub fn concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency.max(1);
        self
    }

    /// Model override for every request.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Send every example and pair each answer with its label. Failed requests are kept aside
    /// in [`EvalRun::errors`] rather than failing the run.
    pub async fn run(self) -> EvalRun<T, L> {
        let (states, labels): (Vec<Value>, Vec<L>) = self.examples.into_iter().map(|e| (e.state, e.label)).unzip();
        let mut batch: Batch<'_, T> = self.client.batch::<T>(states).concurrency(self.concurrency);
        if let Some(model) = self.model {
            batch = batch.model(model);
        }
        let outcome = batch.run().await;
        let mut results = Vec::new();
        let mut errors = Vec::new();
        for (index, (result, label)) in outcome.results.into_iter().zip(labels).enumerate() {
            match result {
                Ok(answered) => results.push((answered, label)),
                Err(error) => errors.push((index, error)),
            }
        }
        EvalRun { results, errors, usage: outcome.usage, elapsed: outcome.elapsed }
    }
}

/// Answers paired with labels, ready for metrics.
#[derive(Debug)]
#[non_exhaustive]
pub struct EvalRun<T, L> {
    /// Each successful example's typed answers and its label.
    pub results: Vec<(Answered<T>, L)>,
    /// Examples whose request failed, by input index.
    pub errors: Vec<(usize, Error)>,
    /// Token usage over the run.
    pub usage: Usage,
    /// Wall-clock time for the run.
    pub elapsed: std::time::Duration,
}

impl<T, L> EvalRun<T, L> {
    /// Evaluate a yes/no question: `probability` reads the noul's probability from the answers.
    pub fn binary(&self, probability: impl Fn(&T) -> f64) -> BinaryReport
    where
        L: Copy + Into<bool>,
    {
        binary(self.results.iter().map(|(answered, label)| (probability(&answered.answers), (*label).into())))
    }

    /// Evaluate a choice question: `predicted` reads the chosen label and its confidence; the
    /// example label is compared by its wire label.
    pub fn choice<P: AsRef<str>>(&self, predicted: impl Fn(&T) -> (P, f64)) -> ChoiceReport
    where
        L: LabelText,
    {
        choice(self.results.iter().map(|(answered, label)| {
            let (p, confidence) = predicted(&answered.answers);
            (p.as_ref().to_string(), confidence, label.label_text())
        }))
    }

    /// Evaluate a score question: `predicted` reads the expected score, the most likely level,
    /// and the confidence; the example label is the true level.
    pub fn score(&self, predicted: impl Fn(&T) -> (f64, u32, f64)) -> ScoreReport
    where
        L: LevelNumber,
    {
        score(self.results.iter().map(|(answered, label)| {
            let (expected, most_likely, confidence) = predicted(&answered.answers);
            (expected, most_likely, confidence, label.level_number())
        }))
    }
}

/// A label that can be compared to a choice answer's wire label.
pub trait LabelText {
    /// The wire label.
    fn label_text(&self) -> String;
}
impl LabelText for String {
    fn label_text(&self) -> String {
        self.clone()
    }
}
impl LabelText for &str {
    fn label_text(&self) -> String {
        (*self).to_string()
    }
}
impl<T: ChoiceLabels> LabelText for T {
    fn label_text(&self) -> String {
        self.label().to_string()
    }
}

/// A label that can be compared to a score answer's level.
pub trait LevelNumber {
    /// The level as an integer.
    fn level_number(&self) -> u32;
}
impl LevelNumber for u32 {
    fn level_number(&self) -> u32 {
        *self
    }
}
impl<T: ScoreLevels> LevelNumber for T {
    fn level_number(&self) -> u32 {
        self.level()
    }
}

impl TypeSafeClient {
    /// Run a question set over labeled examples; see the [`eval`](crate::eval) module.
    pub fn evaluate<T: Questions, L>(
        &self,
        examples: impl IntoIterator<Item = impl Into<Example<L>>>,
    ) -> Evaluate<'_, T, L> {
        Evaluate {
            client: self,
            examples: examples.into_iter().map(Into::into).collect(),
            concurrency: 4,
            model: None,
            _answers: std::marker::PhantomData,
        }
    }
}

// --- Binary metrics -----------------------------------------------------------------------------

/// One row of a calibration table.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct CalibrationBin {
    /// Lower bound of the predicted-probability bin (inclusive).
    pub lower: f64,
    /// Upper bound of the bin (exclusive, except the last bin).
    pub upper: f64,
    /// Examples in the bin.
    pub count: usize,
    /// Mean predicted probability in the bin.
    pub mean_predicted: f64,
    /// Fraction of positives in the bin (or accuracy, for confidence bins).
    pub observed: f64,
}

/// Metrics at one decision threshold.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ThresholdPoint {
    /// Predict yes at or above this probability.
    pub threshold: f64,
    /// Fraction of all examples predicted yes.
    pub positive_rate: f64,
    /// Correct predictions over all examples.
    pub accuracy: f64,
    /// True positives over predicted positives (1 when nothing is predicted positive).
    pub precision: f64,
    /// True positives over actual positives (1 when there are no positives).
    pub recall: f64,
    /// Harmonic mean of precision and recall.
    pub f1: f64,
}

/// How a yes/no question performs against labels.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct BinaryReport {
    /// Examples evaluated.
    pub n: usize,
    /// Examples labeled yes.
    pub positives: usize,
    /// Mean squared error of the probabilities; 0 is perfect, 0.25 is always saying 0.5.
    pub brier: f64,
    /// Mean negative log likelihood; lower is better.
    pub log_loss: f64,
    /// Area under the ROC curve; 0.5 is chance, 1 is perfect ranking.
    pub auc: f64,
    /// Expected calibration error over ten equal-width bins; lower is better.
    pub ece: f64,
    /// Ten equal-width calibration bins (empty bins have `count` 0).
    pub calibration: Vec<CalibrationBin>,
    /// Metrics at thresholds from 0.05 to 0.95.
    pub thresholds: Vec<ThresholdPoint>,
    /// The threshold with the highest accuracy.
    pub best_accuracy: ThresholdPoint,
    /// The threshold with the highest F1.
    pub best_f1: ThresholdPoint,
}

/// Compute [`BinaryReport`] from `(probability of yes, actual)` pairs.
pub fn binary(predictions: impl IntoIterator<Item = (f64, bool)>) -> BinaryReport {
    let pairs: Vec<(f64, bool)> = predictions.into_iter().map(|(p, y)| (p.clamp(0.0, 1.0), y)).collect();
    let n = pairs.len();
    let positives = pairs.iter().filter(|(_, y)| *y).count();
    let brier = mean(pairs.iter().map(|(p, y)| (p - f64::from(u8::from(*y))).powi(2)));
    let log_loss = mean(pairs.iter().map(|(p, y)| {
        let p = p.clamp(1e-12, 1.0 - 1e-12);
        if *y { -p.ln() } else { -(1.0 - p).ln() }
    }));
    let auc = auc(&pairs);
    let calibration = calibration_bins(pairs.iter().map(|(p, y)| (*p, *y)));
    let ece = expected_calibration_error(&calibration, n);
    let thresholds: Vec<ThresholdPoint> = (1..20).map(|i| threshold_point(&pairs, f64::from(i) * 0.05)).collect();
    let best_accuracy = thresholds
        .iter()
        .cloned()
        .max_by(|a, b| a.accuracy.total_cmp(&b.accuracy))
        .unwrap_or_else(|| threshold_point(&pairs, 0.5));
    let best_f1 =
        thresholds.iter().cloned().max_by(|a, b| a.f1.total_cmp(&b.f1)).unwrap_or_else(|| threshold_point(&pairs, 0.5));
    BinaryReport { n, positives, brier, log_loss, auc, ece, calibration, thresholds, best_accuracy, best_f1 }
}

fn mean(values: impl Iterator<Item = f64>) -> f64 {
    let (sum, count) = values.fold((0.0, 0usize), |(s, c), v| (s + v, c + 1));
    if count == 0 { 0.0 } else { sum / count as f64 }
}

/// Mann-Whitney AUC with average ranks for ties.
fn auc(pairs: &[(f64, bool)]) -> f64 {
    let positives = pairs.iter().filter(|(_, y)| *y).count();
    let negatives = pairs.len() - positives;
    if positives == 0 || negatives == 0 {
        return 0.5;
    }
    let mut sorted: Vec<(f64, bool)> = pairs.to_vec();
    sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut rank_sum = 0.0;
    let mut i = 0;
    while i < sorted.len() {
        let mut j = i;
        while j + 1 < sorted.len() && sorted[j + 1].0 == sorted[i].0 {
            j += 1;
        }
        let average_rank = (i + j) as f64 / 2.0 + 1.0;
        for item in &sorted[i..=j] {
            if item.1 {
                rank_sum += average_rank;
            }
        }
        i = j + 1;
    }
    let p = positives as f64;
    (rank_sum - p * (p + 1.0) / 2.0) / (p * negatives as f64)
}

fn calibration_bins(pairs: impl Iterator<Item = (f64, bool)>) -> Vec<CalibrationBin> {
    let mut sums = vec![(0usize, 0.0f64, 0usize); 10];
    for (p, y) in pairs {
        let bin = ((p * 10.0).floor() as usize).min(9);
        sums[bin].0 += 1;
        sums[bin].1 += p;
        sums[bin].2 += usize::from(y);
    }
    sums.into_iter()
        .enumerate()
        .map(|(i, (count, p_sum, hits))| CalibrationBin {
            lower: i as f64 / 10.0,
            upper: (i + 1) as f64 / 10.0,
            count,
            mean_predicted: if count == 0 { 0.0 } else { p_sum / count as f64 },
            observed: if count == 0 { 0.0 } else { hits as f64 / count as f64 },
        })
        .collect()
}

fn expected_calibration_error(bins: &[CalibrationBin], n: usize) -> f64 {
    if n == 0 {
        return 0.0;
    }
    bins.iter().map(|b| (b.count as f64 / n as f64) * (b.observed - b.mean_predicted).abs()).sum()
}

fn threshold_point(pairs: &[(f64, bool)], threshold: f64) -> ThresholdPoint {
    let n = pairs.len();
    let (mut tp, mut fp, mut fn_, mut tn) = (0usize, 0usize, 0usize, 0usize);
    for (p, y) in pairs {
        match (*p >= threshold, *y) {
            (true, true) => tp += 1,
            (true, false) => fp += 1,
            (false, true) => fn_ += 1,
            (false, false) => tn += 1,
        }
    }
    let ratio = |a: usize, b: usize| if b == 0 { 1.0 } else { a as f64 / b as f64 };
    let precision = ratio(tp, tp + fp);
    let recall = ratio(tp, tp + fn_);
    let f1 = if precision + recall == 0.0 { 0.0 } else { 2.0 * precision * recall / (precision + recall) };
    ThresholdPoint {
        threshold,
        positive_rate: if n == 0 { 0.0 } else { (tp + fp) as f64 / n as f64 },
        accuracy: if n == 0 { 0.0 } else { (tp + tn) as f64 / n as f64 },
        precision,
        recall,
        f1,
    }
}

impl fmt::Display for BinaryReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "n={} positives={} brier={:.4} log_loss={:.4} auc={:.3} ece={:.3}",
            self.n, self.positives, self.brier, self.log_loss, self.auc, self.ece
        )?;
        writeln!(
            f,
            "best accuracy {:.3} at threshold {:.2}; best F1 {:.3} at threshold {:.2}",
            self.best_accuracy.accuracy, self.best_accuracy.threshold, self.best_f1.f1, self.best_f1.threshold
        )?;
        writeln!(f, "calibration (predicted -> observed):")?;
        for bin in self.calibration.iter().filter(|b| b.count > 0) {
            writeln!(
                f,
                "  [{:.1}, {:.1})  n={:<5} predicted={:.3} observed={:.3}",
                bin.lower, bin.upper, bin.count, bin.mean_predicted, bin.observed
            )?;
        }
        Ok(())
    }
}

// --- Choice metrics -----------------------------------------------------------------------------

/// Precision, recall, and support for one label.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct LabelStats {
    /// Correct predictions of this label over all predictions of it.
    pub precision: f64,
    /// Correct predictions of this label over all examples labeled it.
    pub recall: f64,
    /// Harmonic mean of precision and recall.
    pub f1: f64,
    /// Examples labeled with this label.
    pub support: usize,
}

/// What happens if answers at or above a confidence are accepted automatically.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct CoveragePoint {
    /// Accept at or above this confidence.
    pub threshold: f64,
    /// Fraction of examples accepted.
    pub accepted: f64,
    /// Accuracy among the accepted examples (1 when none are accepted).
    pub accepted_accuracy: f64,
}

/// How a choice question performs against labels.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ChoiceReport {
    /// Examples evaluated.
    pub n: usize,
    /// Correct predictions over all examples.
    pub accuracy: f64,
    /// Expected calibration error of the confidence against correctness.
    pub ece: f64,
    /// Per-label statistics, keyed by label.
    pub per_label: BTreeMap<String, LabelStats>,
    /// Confusion counts keyed by `(actual, predicted)`.
    pub confusion: Vec<((String, String), usize)>,
    /// Calibration of confidence against correctness, ten bins.
    pub calibration: Vec<CalibrationBin>,
    /// Coverage at confidence thresholds from 0.50 to 1.00.
    pub coverage: Vec<CoveragePoint>,
}

impl ChoiceReport {
    /// The lowest confidence threshold whose accepted answers reach `min_accuracy`, as a
    /// [`Gate`] that accepts at that confidence and reviews everything else; `None` if no
    /// threshold reaches it.
    pub fn suggest_gate(&self, min_accuracy: f64) -> Option<Gate> {
        self.coverage
            .iter()
            .find(|c| c.accepted > 0.0 && c.accepted_accuracy >= min_accuracy)
            .map(|c| Gate::accept_or_review(c.threshold))
    }
}

/// Compute [`ChoiceReport`] from `(predicted label, confidence, actual label)` triples.
pub fn choice(predictions: impl IntoIterator<Item = (String, f64, String)>) -> ChoiceReport {
    let rows: Vec<(String, f64, String)> = predictions.into_iter().collect();
    let n = rows.len();
    let correct = |row: &(String, f64, String)| row.0 == row.2;
    let accuracy = if n == 0 { 0.0 } else { rows.iter().filter(|r| correct(r)).count() as f64 / n as f64 };

    let mut labels: Vec<String> = rows.iter().flat_map(|r| [r.0.clone(), r.2.clone()]).collect();
    labels.sort();
    labels.dedup();
    let per_label = labels
        .iter()
        .map(|label| {
            let tp = rows.iter().filter(|r| &r.0 == label && &r.2 == label).count();
            let predicted = rows.iter().filter(|r| &r.0 == label).count();
            let support = rows.iter().filter(|r| &r.2 == label).count();
            let precision = if predicted == 0 { 1.0 } else { tp as f64 / predicted as f64 };
            let recall = if support == 0 { 1.0 } else { tp as f64 / support as f64 };
            let f1 = if precision + recall == 0.0 { 0.0 } else { 2.0 * precision * recall / (precision + recall) };
            (label.clone(), LabelStats { precision, recall, f1, support })
        })
        .collect();

    let mut confusion: BTreeMap<(String, String), usize> = BTreeMap::new();
    for row in &rows {
        *confusion.entry((row.2.clone(), row.0.clone())).or_default() += 1;
    }

    let calibration = calibration_bins(rows.iter().map(|r| (r.1.clamp(0.0, 1.0), correct(r))));
    let ece = expected_calibration_error(&calibration, n);
    let coverage = (10..=20)
        .map(|i| {
            let threshold = f64::from(i) * 0.05;
            let accepted: Vec<&(String, f64, String)> = rows.iter().filter(|r| r.1 >= threshold).collect();
            CoveragePoint {
                threshold,
                accepted: if n == 0 { 0.0 } else { accepted.len() as f64 / n as f64 },
                accepted_accuracy: if accepted.is_empty() {
                    1.0
                } else {
                    accepted.iter().filter(|r| correct(r)).count() as f64 / accepted.len() as f64
                },
            }
        })
        .collect();

    ChoiceReport { n, accuracy, ece, per_label, confusion: confusion.into_iter().collect(), calibration, coverage }
}

impl fmt::Display for ChoiceReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "n={} accuracy={:.3} ece={:.3}", self.n, self.accuracy, self.ece)?;
        for (label, stats) in &self.per_label {
            writeln!(
                f,
                "  {label:<16} precision={:.3} recall={:.3} f1={:.3} support={}",
                stats.precision, stats.recall, stats.f1, stats.support
            )?;
        }
        writeln!(f, "coverage (accept at confidence -> fraction accepted, accuracy among accepted):")?;
        for point in &self.coverage {
            writeln!(
                f,
                "  >= {:.2}  accepted={:.3} accuracy={:.3}",
                point.threshold, point.accepted, point.accepted_accuracy
            )?;
        }
        Ok(())
    }
}

// --- Score metrics ------------------------------------------------------------------------------

/// How a score question performs against labeled levels.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ScoreReport {
    /// Examples evaluated.
    pub n: usize,
    /// Mean absolute error of the expected score against the true level.
    pub mae: f64,
    /// Root mean squared error of the expected score against the true level.
    pub rmse: f64,
    /// Fraction where the most likely level equals the true level.
    pub exact: f64,
    /// Fraction where the most likely level is within one of the true level.
    pub within_one: f64,
    /// Expected calibration error of the confidence against exact agreement.
    pub ece: f64,
    /// Calibration of confidence against exact agreement, ten bins.
    pub calibration: Vec<CalibrationBin>,
    /// Coverage of exact agreement at confidence thresholds from 0.50 to 1.00.
    pub coverage: Vec<CoveragePoint>,
}

impl ScoreReport {
    /// The lowest confidence threshold whose accepted answers reach `min_exact` agreement.
    pub fn suggest_gate(&self, min_exact: f64) -> Option<Gate> {
        self.coverage
            .iter()
            .find(|c| c.accepted > 0.0 && c.accepted_accuracy >= min_exact)
            .map(|c| Gate::accept_or_review(c.threshold))
    }
}

/// Compute [`ScoreReport`] from `(expected score, most likely level, confidence, actual level)`.
pub fn score(predictions: impl IntoIterator<Item = (f64, u32, f64, u32)>) -> ScoreReport {
    let rows: Vec<(f64, u32, f64, u32)> = predictions.into_iter().collect();
    let n = rows.len();
    let mae = mean(rows.iter().map(|r| (r.0 - f64::from(r.3)).abs()));
    let rmse = mean(rows.iter().map(|r| (r.0 - f64::from(r.3)).powi(2))).sqrt();
    let exact_hits = |r: &(f64, u32, f64, u32)| r.1 == r.3;
    let exact = mean(rows.iter().map(|r| f64::from(u8::from(exact_hits(r)))));
    let within_one = mean(rows.iter().map(|r| f64::from(u8::from(r.1.abs_diff(r.3) <= 1))));
    let calibration = calibration_bins(rows.iter().map(|r| (r.2.clamp(0.0, 1.0), exact_hits(r))));
    let ece = expected_calibration_error(&calibration, n);
    let coverage = (10..=20)
        .map(|i| {
            let threshold = f64::from(i) * 0.05;
            let accepted: Vec<_> = rows.iter().filter(|r| r.2 >= threshold).collect();
            CoveragePoint {
                threshold,
                accepted: if n == 0 { 0.0 } else { accepted.len() as f64 / n as f64 },
                accepted_accuracy: if accepted.is_empty() {
                    1.0
                } else {
                    accepted.iter().filter(|r| exact_hits(r)).count() as f64 / accepted.len() as f64
                },
            }
        })
        .collect();
    ScoreReport { n, mae, rmse, exact, within_one, ece, calibration, coverage }
}

impl fmt::Display for ScoreReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "n={} mae={:.3} rmse={:.3} exact={:.3} within_one={:.3} ece={:.3}",
            self.n, self.mae, self.rmse, self.exact, self.within_one, self.ece
        )?;
        for point in &self.coverage {
            writeln!(
                f,
                "  >= {:.2}  accepted={:.3} exact={:.3}",
                point.threshold, point.accepted, point.accepted_accuracy
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_metrics() {
        let report = binary([(0.9, true), (0.8, true), (0.3, false), (0.2, false), (0.6, false), (0.4, true)]);
        assert_eq!(report.n, 6);
        assert_eq!(report.positives, 3);
        assert!(report.auc > 0.7 && report.auc < 0.9, "{}", report.auc);
        assert!(report.brier < 0.2);
        assert_eq!(report.calibration.len(), 10);
        assert_eq!(report.calibration[9].count, 1);
        assert_eq!(report.thresholds.len(), 19);
        let at_half = report.thresholds.iter().find(|t| (t.threshold - 0.5).abs() < 1e-9).unwrap();
        assert!((at_half.accuracy - 4.0 / 6.0).abs() < 1e-9);
        assert!(report.best_f1.f1 >= at_half.f1);
        assert!(report.to_string().contains("auc="));

        let perfect = binary([(1.0, true), (0.0, false)]);
        assert_eq!(perfect.auc, 1.0);
        assert_eq!(perfect.brier, 0.0);
        assert_eq!(perfect.ece, 0.0);
        let empty = binary(Vec::<(f64, bool)>::new());
        assert_eq!(empty.n, 0);
        assert_eq!(empty.auc, 0.5);
        let ties = binary([(0.5, true), (0.5, false)]);
        assert_eq!(ties.auc, 0.5);
    }

    #[test]
    fn choice_metrics_and_gate() {
        let rows = vec![
            ("a".to_string(), 0.95, "a".to_string()),
            ("a".to_string(), 0.9, "a".to_string()),
            ("b".to_string(), 0.6, "a".to_string()),
            ("b".to_string(), 0.85, "b".to_string()),
            ("c".to_string(), 0.55, "b".to_string()),
        ];
        let report = choice(rows);
        assert_eq!(report.n, 5);
        assert!((report.accuracy - 0.6).abs() < 1e-9);
        assert_eq!(report.per_label["a"].support, 3);
        assert!((report.per_label["a"].precision - 1.0).abs() < 1e-9);
        assert!((report.per_label["a"].recall - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(report.per_label["c"].support, 0);
        assert_eq!(report.confusion.iter().find(|((a, p), _)| a == "a" && p == "b").map(|(_, n)| *n), Some(1));
        let gate = report.suggest_gate(0.99).unwrap();
        // Everything at >= 0.65 is correct; 0.60 admits a wrong answer, so the gate sits between.
        assert_eq!(gate.evaluate(0.7), crate::decision::Outcome::Accept);
        assert_eq!(gate.evaluate(0.6), crate::decision::Outcome::Review);
        assert!(report.suggest_gate(1.01).is_none());
        assert!(report.to_string().contains("accuracy=0.600"));
    }

    #[test]
    fn score_metrics() {
        let report = score([(2.0, 2, 0.9, 2), (1.2, 1, 0.6, 2), (0.1, 0, 0.95, 0), (2.9, 3, 0.7, 1)]);
        assert_eq!(report.n, 4);
        assert!((report.exact - 0.5).abs() < 1e-9);
        assert!((report.within_one - 0.75).abs() < 1e-9);
        assert!((report.mae - (0.0 + 0.8 + 0.1 + 1.9) / 4.0).abs() < 1e-9);
        assert!(report.suggest_gate(0.99).is_some());
        assert!(report.to_string().contains("mae="));
    }
}
