//! Typed questions and answers: Rust enums as choice labels and score rubrics, and structs as
//! whole question sets.
//!
//! With the `derive` feature (on by default) the traits here are implemented for you:
//!
//! ```
//! use typesafeai_sdk_community::{ChoiceLabels, Questions, ScoreLevels};
//! use typesafeai_sdk_community::typed::{TypedChoice, TypedScore};
//! use typesafeai_sdk_community::NoulAnswer;
//!
//! #[derive(Clone, Copy, Debug, PartialEq, Eq, ChoiceLabels)]
//! enum Tone {
//!     #[choice(describe = "An upset or hostile message")]
//!     Angry,
//!     #[choice(describe = "A neutral or polite message")]
//!     Calm,
//!     Excited,
//! }
//!
//! #[derive(Clone, Copy, Debug, PartialEq, Eq, ScoreLevels)]
//! enum Urgency {
//!     #[score("Can wait")]
//!     Low,
//!     #[score("Needs attention this week")]
//!     Medium,
//!     #[score("Needs attention today")]
//!     High,
//! }
//!
//! #[derive(Debug, Questions)]
//! struct Triage {
//!     #[noul("Is this message about billing?")]
//!     billing: NoulAnswer,
//!     #[choice("What is the tone of the message?")]
//!     tone: TypedChoice<Tone>,
//!     #[score("How urgent is the message?")]
//!     urgency: TypedScore<Urgency>,
//!     #[noul("Does the customer ask for a refund?")]
//!     refund: bool,
//! }
//!
//! # use typesafeai_sdk_community::typed::Questions as _;
//! let questions = Triage::questions();
//! assert_eq!(questions.len(), 4);
//! ```
//!
//! Then `client.ask::<Triage>(state).send().await?` sends every field as one request and
//! returns a `Triage`. Field types decide what you get back:
//!
//! | Question | Field type | You get |
//! | --- | --- | --- |
//! | `#[noul]` | [`NoulAnswer`] | the probability object |
//! | `#[noul]` | `f64` | the probability |
//! | `#[noul]` | `bool` | `probability >= 0.5` |
//! | `#[choice]` | [`ChoiceAnswer`] | the untyped answer (needs `labels = [...]`) |
//! | `#[choice]` | [`TypedChoice<T>`] | the winning `T` plus typed probabilities |
//! | `#[choice]` | `T: ChoiceLabels` | just the winning `T` |
//! | `#[score]` | [`ScoreAnswer`] | the untyped answer (needs `levels = [...]`) |
//! | `#[score]` | [`TypedScore<T>`] | expected score, most likely `T`, typed probabilities |
//! | `#[score]` | `T: ScoreLevels` | just the most likely `T` |
//! | `#[score]` | `f64` | the expected score (needs `levels = [...]`) |
//! | any | [`Answer`] | the raw answer (choice/score need `labels`/`levels`) |
//! | any | `Option<...>` | `None` when the question is missing from the response |
//!
//! Putting a `#[choice]` attribute on a `NoulAnswer` field (or any other mismatch) is a compile
//! error:
//!
//! ```compile_fail
//! use typesafeai_sdk_community::{NoulAnswer, Questions};
//! #[derive(Questions)]
//! struct Bad {
//!     #[choice("Tone?", labels = ["a"])]
//!     tone: NoulAnswer,
//! }
//! ```
//!
//! So is a field without a question attribute, or a labels enum with data-carrying variants:
//!
//! ```compile_fail
//! use typesafeai_sdk_community::{NoulAnswer, Questions};
//! #[derive(Questions)]
//! struct Bad {
//!     tone: NoulAnswer,
//! }
//! ```
//!
//! ```compile_fail
//! use typesafeai_sdk_community::ChoiceLabels;
//! #[derive(Clone, Copy, Debug, PartialEq, Eq, ChoiceLabels)]
//! enum Bad { A(u8) }
//! ```
//!
//! `ChoiceLabels` and `ScoreLevels` reject generic enums with a clear message, and a fixture
//! builder cannot have a field named after one of its own methods:
//!
//! ```compile_fail
//! use typesafeai_sdk_community::ChoiceLabels;
//! #[derive(Clone, Copy, Debug, PartialEq, Eq, ChoiceLabels)]
//! enum Bad<const N: usize> { A, B }
//! ```
//!
//! ```compile_fail
//! use typesafeai_sdk_community::{NoulAnswer, Questions};
//! #[derive(Questions)]
//! #[questions(mock)]
//! struct Bad {
//!     #[noul("Built?")]
//!     build: NoulAnswer,
//! }
//! ```

use std::collections::BTreeMap;
use std::fmt;

use serde_json::Value;

use crate::error::AnswerError;
use crate::question::{Choice, Question, Score};
use crate::response::{Answer, ChoiceAnswer, NoulAnswer, ScoreAnswer, SystemOneResponse};

// --- Labels and levels ------------------------------------------------------------------------

/// A unit enum whose variants are the labels of a choice question.
///
/// Implement with `#[derive(ChoiceLabels)]`; the enum must also be `Copy + Eq + Debug`.
pub trait ChoiceLabels: Copy + Eq + fmt::Debug + 'static {
    /// Every variant, in declaration order.
    const ALL: &'static [Self];

    /// The wire label for this variant.
    fn label(self) -> &'static str;

    /// The description sent as this label's criteria, if any.
    fn describe(self) -> Option<Value>;

    /// The variant for a wire label, if it is one.
    fn from_label(label: &str) -> Option<Self>;

    /// The criteria map for a choice question over these labels.
    fn criteria() -> BTreeMap<String, Option<Value>> {
        Self::ALL.iter().map(|variant| (variant.label().to_string(), variant.describe())).collect()
    }
}

/// A unit enum whose declaration order is the rubric of a score question, scored from zero.
///
/// Implement with `#[derive(ScoreLevels)]`; the enum must also be `Copy + Eq + Debug`.
pub trait ScoreLevels: Copy + Eq + fmt::Debug + 'static {
    /// Every variant, in level order.
    const ALL: &'static [Self];

    /// The integer score of this level.
    fn level(self) -> u32;

    /// The rubric description of this level.
    fn describe(self) -> Value;

    /// The variant for an integer score, if it is one.
    fn from_level(level: u32) -> Option<Self>;

    /// The highest level.
    fn max_level() -> u32 {
        Self::ALL.len().saturating_sub(1) as u32
    }

    /// The ordered criteria for a score question over these levels.
    fn criteria() -> Vec<Value> {
        Self::ALL.iter().map(|variant| variant.describe()).collect()
    }
}

impl Choice {
    /// A choice question whose labels come from a [`ChoiceLabels`] enum.
    pub fn of<T: ChoiceLabels>(instructions: impl Into<Value>) -> Self {
        Choice { instructions: Some(instructions.into()), criteria: T::criteria() }
    }
}

impl Score {
    /// A score question whose rubric comes from a [`ScoreLevels`] enum.
    pub fn of<T: ScoreLevels>(instructions: impl Into<Value>) -> Self {
        Score { instructions: Some(instructions.into()), criteria: T::criteria() }
    }
}

// --- Typed answers ------------------------------------------------------------------------------

/// A choice answer whose labels are a [`ChoiceLabels`] enum.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct TypedChoice<T: ChoiceLabels> {
    /// The label with the highest probability.
    pub choice: T,
    /// Confidence in the selected label, from 0 to 1.
    pub confidence: f64,
    /// Every label's probability, sorted from most to least likely.
    pub probabilities: Vec<(T, f64)>,
}

impl<T: ChoiceLabels> TypedChoice<T> {
    /// A typed choice from its parts; probabilities are sorted from most to least likely.
    pub fn new(choice: T, confidence: f64, probabilities: impl IntoIterator<Item = (T, f64)>) -> Self {
        let mut probabilities: Vec<(T, f64)> = probabilities.into_iter().collect();
        probabilities.sort_by(|a, b| b.1.total_cmp(&a.1));
        TypedChoice { choice, confidence, probabilities }
    }

    /// Convert an untyped answer, failing on any label the enum does not know.
    pub fn from_answer(name: &str, answer: &ChoiceAnswer) -> Result<Self, AnswerError> {
        let unknown = |label: &str| AnswerError::UnknownLabel { name: name.to_string(), label: label.to_string() };
        let choice = T::from_label(&answer.choice).ok_or_else(|| unknown(&answer.choice))?;
        let mut probabilities = Vec::with_capacity(answer.probabilities.len());
        for (label, probability) in &answer.probabilities {
            probabilities.push((T::from_label(label).ok_or_else(|| unknown(label))?, *probability));
        }
        Ok(TypedChoice::new(choice, answer.confidence, probabilities))
    }

    /// The probability of a label, or 0 if the response did not include it.
    pub fn probability(&self, label: T) -> f64 {
        self.probabilities.iter().find(|(l, _)| *l == label).map_or(0.0, |(_, p)| *p)
    }

    /// The `n` most likely labels.
    pub fn top(&self, n: usize) -> &[(T, f64)] {
        &self.probabilities[..n.min(self.probabilities.len())]
    }

    /// The gap between the most likely label and the runner-up, from 0 to 1.
    pub fn margin(&self) -> f64 {
        crate::decision::margin(self.probabilities.iter().map(|(_, p)| *p))
    }

    /// Classify this answer's confidence with a [`Gate`](crate::decision::Gate).
    pub fn gate(&self, gate: crate::decision::Gate) -> crate::decision::Outcome {
        gate.evaluate(self.confidence)
    }
}

/// A score answer whose rubric is a [`ScoreLevels`] enum.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct TypedScore<T: ScoreLevels> {
    /// Expected score: the probability-weighted average of the levels.
    pub score: f64,
    /// Confidence in the score, from 0 to 1.
    pub confidence: f64,
    /// The level with the highest probability.
    pub most_likely: T,
    /// Every level's probability, in level order.
    pub probabilities: Vec<(T, f64)>,
}

impl<T: ScoreLevels> TypedScore<T> {
    /// A typed score from its parts; probabilities are sorted by level.
    pub fn new(score: f64, confidence: f64, most_likely: T, probabilities: impl IntoIterator<Item = (T, f64)>) -> Self {
        let mut probabilities: Vec<(T, f64)> = probabilities.into_iter().collect();
        probabilities.sort_by_key(|(level, _)| level.level());
        TypedScore { score, confidence, most_likely, probabilities }
    }

    /// Convert an untyped answer, failing on any level the enum does not know.
    pub fn from_answer(name: &str, answer: &ScoreAnswer) -> Result<Self, AnswerError> {
        let unknown = |level: u32| AnswerError::UnknownLevel { name: name.to_string(), level };
        let mut probabilities = Vec::with_capacity(answer.probabilities.len());
        for (level, probability) in &answer.probabilities {
            probabilities.push((T::from_level(*level).ok_or_else(|| unknown(*level))?, *probability));
        }
        let most_likely = probabilities
            .iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(level, _)| *level)
            .or_else(|| T::from_level(answer.score.round().max(0.0) as u32))
            .ok_or_else(|| unknown(answer.score.round().max(0.0) as u32))?;
        Ok(TypedScore::new(answer.score, answer.confidence, most_likely, probabilities))
    }

    /// The probability of a level, or 0 if the response did not include it.
    pub fn probability(&self, level: T) -> f64 {
        self.probabilities.iter().find(|(l, _)| *l == level).map_or(0.0, |(_, p)| *p)
    }

    /// The probability that the score is at least `level`.
    pub fn probability_at_least(&self, level: T) -> f64 {
        self.probabilities.iter().filter(|(l, _)| l.level() >= level.level()).map(|(_, p)| *p).sum()
    }

    /// The probability that the score is at most `level`.
    pub fn probability_at_most(&self, level: T) -> f64 {
        self.probabilities.iter().filter(|(l, _)| l.level() <= level.level()).map(|(_, p)| *p).sum()
    }

    /// The expected score scaled to 0..1 by the rubric's highest level.
    pub fn normalized(&self) -> f64 {
        let max = T::max_level();
        if max == 0 { 0.0 } else { (self.score / f64::from(max)).clamp(0.0, 1.0) }
    }

    /// The gap between the most likely level and the runner-up, from 0 to 1.
    pub fn margin(&self) -> f64 {
        crate::decision::margin(self.probabilities.iter().map(|(_, p)| *p))
    }

    /// Classify this answer's confidence with a [`Gate`](crate::decision::Gate).
    pub fn gate(&self, gate: crate::decision::Gate) -> crate::decision::Outcome {
        gate.evaluate(self.confidence)
    }
}

// --- Conversion from raw answers ----------------------------------------------------------------

/// A field type that can be built from one answer in a response.
///
/// Implemented for the answer structs, the typed wrappers, `bool`, `f64`, [`Answer`], any
/// derived [`ChoiceLabels`] / [`ScoreLevels`] enum, and `Option` of all of those.
pub trait FromAnswer: Sized {
    /// Build from the named answer, or from `None` when the response did not include it.
    fn from_answer(name: &str, answer: Option<&Answer>) -> Result<Self, AnswerError>;
}

fn require<'a>(name: &str, answer: Option<&'a Answer>) -> Result<&'a Answer, AnswerError> {
    answer.ok_or_else(|| AnswerError::Missing { name: name.to_string() })
}

fn wrong(name: &str, expected: &'static str, answer: &Answer) -> AnswerError {
    AnswerError::WrongType { name: name.to_string(), expected, actual: answer.type_name() }
}

impl FromAnswer for Answer {
    fn from_answer(name: &str, answer: Option<&Answer>) -> Result<Self, AnswerError> {
        require(name, answer).cloned()
    }
}

impl FromAnswer for NoulAnswer {
    fn from_answer(name: &str, answer: Option<&Answer>) -> Result<Self, AnswerError> {
        let answer = require(name, answer)?;
        answer.as_noul().cloned().ok_or_else(|| wrong(name, "noul", answer))
    }
}

impl FromAnswer for ChoiceAnswer {
    fn from_answer(name: &str, answer: Option<&Answer>) -> Result<Self, AnswerError> {
        let answer = require(name, answer)?;
        answer.as_choice().cloned().ok_or_else(|| wrong(name, "choice", answer))
    }
}

impl FromAnswer for ScoreAnswer {
    fn from_answer(name: &str, answer: Option<&Answer>) -> Result<Self, AnswerError> {
        let answer = require(name, answer)?;
        answer.as_score().cloned().ok_or_else(|| wrong(name, "score", answer))
    }
}

impl<T: ChoiceLabels> FromAnswer for TypedChoice<T> {
    fn from_answer(name: &str, answer: Option<&Answer>) -> Result<Self, AnswerError> {
        let answer = require(name, answer)?;
        let choice = answer.as_choice().ok_or_else(|| wrong(name, "choice", answer))?;
        TypedChoice::from_answer(name, choice)
    }
}

impl<T: ScoreLevels> FromAnswer for TypedScore<T> {
    fn from_answer(name: &str, answer: Option<&Answer>) -> Result<Self, AnswerError> {
        let answer = require(name, answer)?;
        let score = answer.as_score().ok_or_else(|| wrong(name, "score", answer))?;
        TypedScore::from_answer(name, score)
    }
}

/// A noul's probability, or a score's expected value.
impl FromAnswer for f64 {
    fn from_answer(name: &str, answer: Option<&Answer>) -> Result<Self, AnswerError> {
        match require(name, answer)? {
            Answer::Noul(noul) => Ok(noul.noul),
            Answer::Score(score) => Ok(score.score),
            other => Err(wrong(name, "noul or score", other)),
        }
    }
}

/// A noul decided at 0.5. Use [`NoulAnswer`] with [`NoulAnswer::decide`] for other thresholds.
impl FromAnswer for bool {
    fn from_answer(name: &str, answer: Option<&Answer>) -> Result<Self, AnswerError> {
        let answer = require(name, answer)?;
        answer.as_noul().map(|noul| noul.noul >= 0.5).ok_or_else(|| wrong(name, "noul", answer))
    }
}

impl<T: FromAnswer> FromAnswer for Option<T> {
    fn from_answer(name: &str, answer: Option<&Answer>) -> Result<Self, AnswerError> {
        match answer {
            None => Ok(None),
            Some(_) => T::from_answer(name, answer).map(Some),
        }
    }
}

// --- Compile-time kind checks -----------------------------------------------------------------

/// Implementation detail of the derive macros; not part of the public API.
#[doc(hidden)]
pub mod __private {
    /// Seals the marker traits: only this crate and its derive macros implement them.
    pub trait Sealed {}
}
use __private::Sealed;

impl Sealed for NoulAnswer {}
impl Sealed for ChoiceAnswer {}
impl Sealed for ScoreAnswer {}
impl Sealed for Answer {}
impl Sealed for bool {}
impl Sealed for f64 {}
impl<T: ChoiceLabels> Sealed for TypedChoice<T> {}
impl<T: ScoreLevels> Sealed for TypedScore<T> {}
impl<T: Sealed> Sealed for Option<T> {}

/// Field types a `#[noul]` question may deserialize into. Sealed: implemented by the SDK's
/// answer types and by the derive macros. The derives add `FieldType: NoulTarget` to the
/// generated impl's `where` clause, which is what turns a mismatched field type into a compile
/// error.
pub trait NoulTarget: Sealed + FromAnswer {}
/// Field types a `#[choice]` question may deserialize into. Sealed.
pub trait ChoiceTarget: Sealed + FromAnswer {}
/// Field types a `#[score]` question may deserialize into. Sealed.
pub trait ScoreTarget: Sealed + FromAnswer {}

impl NoulTarget for NoulAnswer {}
impl NoulTarget for bool {}
impl NoulTarget for f64 {}
impl NoulTarget for Answer {}
impl<T: NoulTarget> NoulTarget for Option<T> {}

impl ChoiceTarget for ChoiceAnswer {}
impl ChoiceTarget for Answer {}
impl<T: ChoiceLabels> ChoiceTarget for TypedChoice<T> {}
impl<T: ChoiceTarget> ChoiceTarget for Option<T> {}

impl ScoreTarget for ScoreAnswer {}
impl ScoreTarget for f64 {}
impl ScoreTarget for Answer {}
impl<T: ScoreLevels> ScoreTarget for TypedScore<T> {}
impl<T: ScoreTarget> ScoreTarget for Option<T> {}

/// Field types that carry their own choice criteria. Sealed.
pub trait ChoiceCriteria: Sealed {
    /// The labels and descriptions to send.
    fn criteria() -> BTreeMap<String, Option<Value>>;
}
/// Field types that carry their own score rubric. Sealed.
pub trait ScoreCriteria: Sealed {
    /// The ordered level descriptions to send.
    fn criteria() -> Vec<Value>;
}

impl<T: ChoiceLabels> ChoiceCriteria for TypedChoice<T> {
    fn criteria() -> BTreeMap<String, Option<Value>> {
        T::criteria()
    }
}
impl<T: ChoiceCriteria> ChoiceCriteria for Option<T> {
    fn criteria() -> BTreeMap<String, Option<Value>> {
        T::criteria()
    }
}
impl<T: ScoreLevels> ScoreCriteria for TypedScore<T> {
    fn criteria() -> Vec<Value> {
        T::criteria()
    }
}
impl<T: ScoreCriteria> ScoreCriteria for Option<T> {
    fn criteria() -> Vec<Value> {
        T::criteria()
    }
}

// --- Question sets --------------------------------------------------------------------------------

/// A struct that is a complete set of questions and their answers.
///
/// Implement with `#[derive(Questions)]`. Send it with `client.ask::<T>(state)` or parse any
/// [`SystemOneResponse`] with [`SystemOneResponse::parse`].
pub trait Questions: Sized {
    /// The questions to send, keyed by wire name.
    fn questions() -> BTreeMap<String, Question>;

    /// Build from a response's answers.
    fn from_response(response: &SystemOneResponse) -> Result<Self, AnswerError>;

    /// The backticked state paths in this set's questions that `state` does not contain; see
    /// the [`state`](crate::state) module.
    fn check_paths(state: &Value) -> Vec<crate::state::PathIssue> {
        crate::state::check(state, &Self::questions())
    }
}

/// A question set whose answers select one variant of an enum and fill its fields: typed
/// routing, or function calling.
///
/// Implement with `#[derive(Route)]` on an enum. One request carries the routing choice plus
/// every variant's field questions (the docs' speculative fan-out); only the selected variant is
/// constructed. Send it with `client.route::<T>(state)`.
pub trait Route: Questions {
    /// The wire name of the routing choice question.
    const ROUTE_NAME: &'static str;

    /// Every variant's label, in declaration order.
    const LABELS: &'static [&'static str];

    /// The label of the variant this value is.
    fn label(&self) -> &'static str;
}

/// A routed value together with the routing choice and the full response.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Routed<T> {
    /// The selected variant with its fields filled.
    pub route: T,
    /// The routing choice: confidence and the probability of every variant.
    pub choice: ChoiceAnswer,
    /// The full response, including every speculative branch's answers.
    pub response: SystemOneResponse,
}

impl<T> std::ops::Deref for Routed<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.route
    }
}

impl<T: Route> Routed<T> {
    /// Build from a response: parse the enum and pull out its routing choice.
    pub fn from_response(response: SystemOneResponse) -> Result<Self, AnswerError> {
        let route = T::from_response(&response)?;
        let choice = ChoiceAnswer::from_answer(T::ROUTE_NAME, response.answer(T::ROUTE_NAME))?;
        Ok(Routed { route, choice, response })
    }

    /// The probability the model assigned to a variant label, or 0 if absent.
    pub fn probability(&self, label: &str) -> f64 {
        self.choice.probability(label)
    }

    /// The gap between the selected variant and the runner-up, from 0 to 1.
    pub fn margin(&self) -> f64 {
        self.choice.margin()
    }

    /// Split into the route and the response.
    pub fn into_parts(self) -> (T, SystemOneResponse) {
        (self.route, self.response)
    }
}

/// The [`ChoiceLabels`] enum behind a choice-shaped field type, used by generated fixture
/// builders. Sealed.
pub trait ChoiceOf: Sealed {
    /// The labels enum.
    type Labels: ChoiceLabels;
}
impl<T: ChoiceLabels> ChoiceOf for TypedChoice<T> {
    type Labels = T;
}
impl<T: ChoiceOf> ChoiceOf for Option<T> {
    type Labels = T::Labels;
}

/// The [`ScoreLevels`] enum behind a score-shaped field type, used by generated fixture
/// builders. Sealed.
pub trait ScoreOf: Sealed {
    /// The levels enum.
    type Levels: ScoreLevels;
}
impl<T: ScoreLevels> ScoreOf for TypedScore<T> {
    type Levels = T;
}
impl<T: ScoreOf> ScoreOf for Option<T> {
    type Levels = T::Levels;
}

/// A parsed question set together with the response it came from.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Answered<T> {
    /// The typed answers.
    pub answers: T,
    /// The full response: model, usage, raw answers, request ID.
    pub response: SystemOneResponse,
}

impl<T> std::ops::Deref for Answered<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.answers
    }
}

impl<T> Answered<T> {
    /// Split into the typed answers and the response.
    pub fn into_parts(self) -> (T, SystemOneResponse) {
        (self.answers, self.response)
    }
}

impl SystemOneResponse {
    /// Parse the answers into a [`Questions`] struct.
    pub fn parse<T: Questions>(&self) -> Result<T, AnswerError> {
        T::from_response(self)
    }

    /// The named choice answer with its labels as `T`.
    pub fn choice_as<T: ChoiceLabels>(&self, name: &str) -> Result<TypedChoice<T>, AnswerError> {
        <TypedChoice<T> as FromAnswer>::from_answer(name, self.answer(name))
    }

    /// The named score answer with its levels as `T`.
    pub fn score_as<T: ScoreLevels>(&self, name: &str) -> Result<TypedScore<T>, AnswerError> {
        <TypedScore<T> as FromAnswer>::from_answer(name, self.answer(name))
    }

    /// Any field type that implements [`FromAnswer`], by question name.
    pub fn get<T: FromAnswer>(&self, name: &str) -> Result<T, AnswerError> {
        T::from_answer(name, self.answer(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Tone {
        Angry,
        Calm,
    }
    impl ChoiceLabels for Tone {
        const ALL: &'static [Self] = &[Tone::Angry, Tone::Calm];
        fn label(self) -> &'static str {
            match self {
                Tone::Angry => "angry",
                Tone::Calm => "calm",
            }
        }
        fn describe(self) -> Option<Value> {
            match self {
                Tone::Angry => Some(json!("Upset")),
                Tone::Calm => None,
            }
        }
        fn from_label(label: &str) -> Option<Self> {
            match label {
                "angry" => Some(Tone::Angry),
                "calm" => Some(Tone::Calm),
                _ => None,
            }
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Urgency {
        Low,
        High,
    }
    impl ScoreLevels for Urgency {
        const ALL: &'static [Self] = &[Urgency::Low, Urgency::High];
        fn level(self) -> u32 {
            match self {
                Urgency::Low => 0,
                Urgency::High => 1,
            }
        }
        fn describe(self) -> Value {
            json!(format!("{self:?}"))
        }
        fn from_level(level: u32) -> Option<Self> {
            match level {
                0 => Some(Urgency::Low),
                1 => Some(Urgency::High),
                _ => None,
            }
        }
    }

    fn choice(choice: &str, probabilities: &[(&str, f64)]) -> Answer {
        Answer::Choice(ChoiceAnswer {
            choice: choice.into(),
            confidence: 0.8,
            probabilities: probabilities.iter().map(|(l, p)| (l.to_string(), *p)).collect(),
        })
    }

    #[test]
    fn criteria_from_enums() {
        let choice = Choice::of::<Tone>("Tone?");
        assert_eq!(serde_json::to_value(choice.criteria).unwrap(), json!({"angry": "Upset", "calm": null}));
        let score = Score::of::<Urgency>("Urgency?");
        assert_eq!(score.criteria, vec![json!("Low"), json!("High")]);
        assert_eq!(Urgency::max_level(), 1);
    }

    #[test]
    fn typed_choice_conversion() {
        let answer = choice("calm", &[("angry", 0.3), ("calm", 0.7)]);
        let typed: TypedChoice<Tone> = TypedChoice::from_answer("tone", answer.as_choice().unwrap()).unwrap();
        assert_eq!(typed.choice, Tone::Calm);
        assert_eq!(typed.probabilities, vec![(Tone::Calm, 0.7), (Tone::Angry, 0.3)]);
        assert_eq!(typed.probability(Tone::Angry), 0.3);
        assert_eq!(typed.top(1), &[(Tone::Calm, 0.7)]);
        assert!((typed.margin() - 0.4).abs() < 1e-9);

        let error = TypedChoice::<Tone>::from_answer("tone", choice("excited", &[]).as_choice().unwrap()).unwrap_err();
        assert!(matches!(error, AnswerError::UnknownLabel { ref label, .. } if label == "excited"));
        let error =
            TypedChoice::<Tone>::from_answer("tone", choice("calm", &[("calm", 1.0), ("x", 0.0)]).as_choice().unwrap())
                .unwrap_err();
        assert!(matches!(error, AnswerError::UnknownLabel { ref label, .. } if label == "x"));
    }

    #[test]
    fn typed_score_conversion() {
        let answer = ScoreAnswer {
            score: 0.8,
            confidence: 0.9,
            legend: BTreeMap::new(),
            probabilities: [(0, 0.2), (1, 0.8)].into(),
        };
        let typed = TypedScore::<Urgency>::from_answer("u", &answer).unwrap();
        assert_eq!(typed.most_likely, Urgency::High);
        assert_eq!(typed.probability_at_least(Urgency::High), 0.8);
        assert_eq!(typed.probability_at_most(Urgency::Low), 0.2);
        assert_eq!(typed.normalized(), 0.8);
        let bad = ScoreAnswer { probabilities: [(5, 1.0)].into(), ..answer };
        assert!(matches!(
            TypedScore::<Urgency>::from_answer("u", &bad),
            Err(AnswerError::UnknownLevel { level: 5, .. })
        ));
    }

    #[test]
    fn from_answer_targets() {
        let noul = Answer::Noul(NoulAnswer { noul: 0.7 });
        assert!(bool::from_answer("q", Some(&noul)).unwrap());
        assert_eq!(f64::from_answer("q", Some(&noul)).unwrap(), 0.7);
        assert_eq!(NoulAnswer::from_answer("q", Some(&noul)).unwrap().noul, 0.7);
        assert!(matches!(bool::from_answer("q", None), Err(AnswerError::Missing { .. })));
        assert_eq!(Option::<bool>::from_answer("q", None).unwrap(), None);
        assert_eq!(Option::<bool>::from_answer("q", Some(&noul)).unwrap(), Some(true));
        let error = ChoiceAnswer::from_answer("q", Some(&noul)).unwrap_err();
        assert!(matches!(error, AnswerError::WrongType { expected: "choice", actual: "noul", .. }));
        assert_eq!(error.to_string(), "Answer \"q\" is a noul answer, but a choice answer was expected.");
        let tone = choice("angry", &[("angry", 0.9), ("calm", 0.1)]);
        assert_eq!(<TypedChoice<Tone> as FromAnswer>::from_answer("q", Some(&tone)).unwrap().choice, Tone::Angry);
        assert_eq!(Answer::from_answer("q", Some(&tone)).unwrap(), tone);
    }
}
