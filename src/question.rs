//! Question objects: the inputs to a System One request.
//!
//! Three primitives are available, each with a builder-style API:
//!
//! - [`Noul`]: whether a condition holds; answered with a probability of "yes".
//! - [`Choice`]: one of a defined set of labels; answered with the winning label and a
//!   probability distribution over all labels.
//! - [`Score`]: a degree along an ordered rubric; answered with an expected score and a
//!   distribution over the levels.
//!
//! Instructions and criteria descriptions accept anything convertible into a
//! [`serde_json::Value`]: plain strings, or structured JSON built with [`serde_json::json!`].
//!
//! ```
//! use typesafeai_sdk_community::{Choice, Noul, Score, json};
//!
//! let billing = Noul::new("Is this message about billing?")
//!     .when_true("The customer mentions charges, invoices, or refunds.")
//!     .when_false("The message is about something else.");
//!
//! let tone = Choice::new("What is the tone of this message?")
//!     .option("angry", "An upset or hostile message")
//!     .option("calm", "A neutral or polite message")
//!     .label("excited");
//!
//! let urgency = Score::new(
//!     json!({"task": "How urgent is this message?"}),
//!     ["Can wait", "Needs attention this week", "Needs attention today"],
//! );
//! ```

use std::collections::BTreeMap;

use serde::de::{self, Deserializer};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::{Error, Result};

// The question structs below are deliberately exhaustive (constructible by struct literal):
// they mirror the request wire schema one-to-one, reject unknown fields, and users build them
// in ordinary code alongside the builder methods. A new wire field would be a deliberate API
// change either way.

/// Optional descriptions of the yes and no outcomes of a [`Noul`] question.
///
/// See the [noul primitive](https://docs.typesafe.ai/primitives/noul) for details.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoulCriteria {
    /// Description of the yes outcome as text, a JSON object, or an array.
    #[serde(rename = "true", default, skip_serializing_if = "Option::is_none")]
    pub when_true: Option<Value>,
    /// Description of the no outcome as text, a JSON object, or an array.
    #[serde(rename = "false", default, skip_serializing_if = "Option::is_none")]
    pub when_false: Option<Value>,
}

/// A yes/no question with optional descriptions for either outcome.
///
/// See the [noul primitive](https://docs.typesafe.ai/primitives/noul) for details.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Noul {
    /// The question to ask, expressed as text, a JSON object, or an array; optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<Value>,
    /// Optional descriptions of the yes and no outcomes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criteria: Option<NoulCriteria>,
}

impl Noul {
    /// A yes/no question with the given instructions.
    pub fn new(instructions: impl Into<Value>) -> Self {
        Noul { instructions: Some(instructions.into()), criteria: None }
    }

    /// Set the instructions.
    pub fn instructions(mut self, instructions: impl Into<Value>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Describe what counts as a yes answer.
    pub fn when_true(mut self, description: impl Into<Value>) -> Self {
        self.criteria.get_or_insert_with(Default::default).when_true = Some(description.into());
        self
    }

    /// Describe what counts as a no answer.
    pub fn when_false(mut self, description: impl Into<Value>) -> Self {
        self.criteria.get_or_insert_with(Default::default).when_false = Some(description.into());
        self
    }

    /// Replace the criteria wholesale.
    pub fn criteria(mut self, criteria: NoulCriteria) -> Self {
        self.criteria = Some(criteria);
        self
    }
}

/// A question that selects between named alternatives.
///
/// See the [choice primitive](https://docs.typesafe.ai/primitives/choice) for details.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    /// The question to ask, expressed as text, a JSON object, or an array; optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<Value>,
    /// Labels mapped to text, object, or array descriptions, or `None` for undescribed labels.
    pub criteria: BTreeMap<String, Option<Value>>,
}

impl Choice {
    /// A choice question with the given instructions and no labels yet; add them with
    /// [`option`](Self::option), [`label`](Self::label), or [`labels`](Self::labels).
    pub fn new(instructions: impl Into<Value>) -> Self {
        Choice { instructions: Some(instructions.into()), criteria: BTreeMap::new() }
    }

    /// A choice question from a complete criteria map, without instructions.
    pub fn from_criteria(criteria: impl IntoIterator<Item = (impl Into<String>, Option<Value>)>) -> Self {
        Choice {
            instructions: None,
            criteria: criteria.into_iter().map(|(label, description)| (label.into(), description)).collect(),
        }
    }

    /// Set the instructions.
    pub fn instructions(mut self, instructions: impl Into<Value>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Add a label with a description of when it applies.
    pub fn option(mut self, label: impl Into<String>, description: impl Into<Value>) -> Self {
        self.criteria.insert(label.into(), Some(description.into()));
        self
    }

    /// Add a label interpreted by its name alone.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.criteria.insert(label.into(), None);
        self
    }

    /// Add several labels interpreted by their names alone.
    pub fn labels(mut self, labels: impl IntoIterator<Item = impl Into<String>>) -> Self {
        for label in labels {
            self.criteria.insert(label.into(), None);
        }
        self
    }
}

/// A question that assigns a score using an ordered rubric.
///
/// See the [score primitive](https://docs.typesafe.ai/primitives/score) for details.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Score {
    /// The question to ask, expressed as text, a JSON object, or an array; optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<Value>,
    /// A nonempty, ordered list of text, object, or array descriptions, one per score from zero.
    pub criteria: Vec<Value>,
}

impl Score {
    /// A score question with the given instructions and ordered levels, scored from zero.
    pub fn new(instructions: impl Into<Value>, levels: impl IntoIterator<Item = impl Into<Value>>) -> Self {
        Score { instructions: Some(instructions.into()), criteria: levels.into_iter().map(Into::into).collect() }
    }

    /// A score question from ordered levels alone, without instructions.
    pub fn from_levels(levels: impl IntoIterator<Item = impl Into<Value>>) -> Self {
        Score { instructions: None, criteria: levels.into_iter().map(Into::into).collect() }
    }

    /// Set the instructions.
    pub fn instructions(mut self, instructions: impl Into<Value>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Append a level to the rubric.
    pub fn level(mut self, description: impl Into<Value>) -> Self {
        self.criteria.push(description.into());
        self
    }
}

/// A question identified by its `type`.
///
/// [`Question::Custom`] carries an arbitrary JSON object that is sent as-is, so question types
/// added by a future API version can be used without an SDK update. It must contain a nonempty
/// string `type`; `choice` and `score` customs must also contain `criteria`.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Question {
    /// A yes/no question.
    Noul(Noul),
    /// A selection between named alternatives.
    Choice(Choice),
    /// A rating on an ordered rubric.
    Score(Score),
    /// A raw question object serialized verbatim.
    Custom(Map<String, Value>),
}

impl Question {
    /// The value of the `type` key this question serializes with.
    pub fn type_name(&self) -> Option<&str> {
        match self {
            Question::Noul(_) => Some("noul"),
            Question::Choice(_) => Some("choice"),
            Question::Score(_) => Some("score"),
            Question::Custom(map) => map.get("type").and_then(Value::as_str),
        }
    }

    /// Check the invariants the Python SDK enforces before encoding.
    pub(crate) fn validate(&self, name: &str) -> Result<()> {
        match self {
            Question::Noul(_) | Question::Choice(_) => Ok(()),
            Question::Score(score) => validate_score_criteria(name, score.criteria.len()),
            Question::Custom(map) => {
                let type_name = map.get("type").and_then(Value::as_str).unwrap_or_default();
                if type_name.is_empty() {
                    return Err(Error::InvalidRequest(format!(
                        "Question {name:?} must be a question object or a JSON object with a nonempty string \"type\"."
                    )));
                }
                if matches!(type_name, "choice" | "score") && !map.contains_key("criteria") {
                    return Err(Error::InvalidRequest(format!("Question {name:?} requires \"criteria\".")));
                }
                if type_name == "score" {
                    let len = map.get("criteria").and_then(Value::as_array).map_or(0, Vec::len);
                    validate_score_criteria(name, len)?;
                }
                Ok(())
            }
        }
    }
}

fn validate_score_criteria(name: &str, len: usize) -> Result<()> {
    if len == 0 {
        return Err(Error::InvalidRequest(format!(
            "Score question {name:?} has no criteria; at least one score is required."
        )));
    }
    Ok(())
}

impl From<Noul> for Question {
    fn from(question: Noul) -> Self {
        Question::Noul(question)
    }
}

impl From<Choice> for Question {
    fn from(question: Choice) -> Self {
        Question::Choice(question)
    }
}

impl From<Score> for Question {
    fn from(question: Score) -> Self {
        Question::Score(question)
    }
}

impl From<Map<String, Value>> for Question {
    fn from(map: Map<String, Value>) -> Self {
        Question::Custom(map)
    }
}

impl Serialize for Question {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self {
            Question::Custom(map) => map.serialize(serializer),
            _ => {
                let type_name = self.type_name().expect("built-in questions have a type");
                let body = match self {
                    Question::Noul(q) => serde_json::to_value(q),
                    Question::Choice(q) => serde_json::to_value(q),
                    Question::Score(q) => serde_json::to_value(q),
                    Question::Custom(_) => unreachable!(),
                }
                .map_err(serde::ser::Error::custom)?;
                let Value::Object(fields) = body else {
                    return Err(serde::ser::Error::custom("question did not serialize to an object"));
                };
                let mut map = serializer.serialize_map(Some(fields.len() + 1))?;
                map.serialize_entry("type", type_name)?;
                for (key, value) in &fields {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Question {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let mut map = Map::<String, Value>::deserialize(deserializer)?;
        let type_name = map.get("type").and_then(Value::as_str).map(str::to_string);
        let parse = |map: Map<String, Value>| Value::Object(map);
        match type_name.as_deref() {
            Some("noul") => {
                map.remove("type");
                serde_json::from_value(parse(map)).map(Question::Noul).map_err(de::Error::custom)
            }
            Some("choice") => {
                map.remove("type");
                serde_json::from_value(parse(map)).map(Question::Choice).map_err(de::Error::custom)
            }
            Some("score") => {
                map.remove("type");
                serde_json::from_value(parse(map)).map(Question::Score).map_err(de::Error::custom)
            }
            Some(_) => Ok(Question::Custom(map)),
            None => Err(de::Error::missing_field("type")),
        }
    }
}

/// Validate a full question set: it must be nonempty and each question must be well-formed.
pub(crate) fn validate_questions(questions: &BTreeMap<String, Question>) -> Result<()> {
    if questions.is_empty() {
        return Err(Error::InvalidRequest("At least one question is required.".into()));
    }
    for (name, question) in questions {
        question.validate(name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn noul_serializes_with_type_and_omits_unset_fields() {
        let q = Question::from(Noul::new("Is this spam?"));
        assert_eq!(serde_json::to_value(&q).unwrap(), json!({"type": "noul", "instructions": "Is this spam?"}));

        let q = Question::from(Noul::default());
        assert_eq!(serde_json::to_value(&q).unwrap(), json!({"type": "noul"}));

        let q = Question::from(Noul::new("Spam?").when_true("Ads").when_false("Legit"));
        assert_eq!(
            serde_json::to_value(&q).unwrap(),
            json!({"type": "noul", "instructions": "Spam?", "criteria": {"true": "Ads", "false": "Legit"}})
        );

        // Explicit nulls inside criteria are preserved.
        let q = Question::from(Noul::new("Spam?").when_true(Value::Null));
        assert_eq!(
            serde_json::to_value(&q).unwrap(),
            json!({"type": "noul", "instructions": "Spam?", "criteria": {"true": null}})
        );
    }

    #[test]
    fn choice_serializes_null_for_undescribed_labels() {
        let q = Question::from(Choice::new("Tone?").option("angry", "Upset").label("calm").labels(["excited"]));
        assert_eq!(
            serde_json::to_value(&q).unwrap(),
            json!({"type": "choice", "instructions": "Tone?", "criteria": {"angry": "Upset", "calm": null, "excited": null}})
        );
        let q = Question::from(Choice::from_criteria([("a", None), ("b", Some(json!({"k": 1})))]));
        assert_eq!(
            serde_json::to_value(&q).unwrap(),
            json!({"type": "choice", "criteria": {"a": null, "b": {"k": 1}}})
        );
    }

    #[test]
    fn score_serializes_levels_in_order() {
        let q = Question::from(Score::new(json!({"task": "Urgency"}), ["low", "high"]).level(json!(["x"])));
        assert_eq!(
            serde_json::to_value(&q).unwrap(),
            json!({"type": "score", "instructions": {"task": "Urgency"}, "criteria": ["low", "high", ["x"]]})
        );
    }

    #[test]
    fn custom_serializes_verbatim() {
        let map = json!({"type": "future", "anything": [1, 2]}).as_object().unwrap().clone();
        let q = Question::from(map.clone());
        assert_eq!(serde_json::to_value(&q).unwrap(), Value::Object(map));
        assert_eq!(q.type_name(), Some("future"));
    }

    #[test]
    fn deserialize_round_trips() {
        let noul: Question = serde_json::from_value(json!({"type": "noul", "instructions": "x"})).unwrap();
        assert_eq!(noul, Question::Noul(Noul::new("x")));
        let choice: Question = serde_json::from_value(json!({"type": "choice", "criteria": {"a": null}})).unwrap();
        assert_eq!(choice, Question::Choice(Choice::from_criteria([("a", None)])));
        let score: Question = serde_json::from_value(json!({"type": "score", "criteria": ["a"]})).unwrap();
        assert_eq!(score, Question::Score(Score::from_levels(["a"])));
        let custom: Question = serde_json::from_value(json!({"type": "other", "x": 1})).unwrap();
        assert!(matches!(custom, Question::Custom(_)));
        assert!(serde_json::from_value::<Question>(json!({"instructions": "x"})).is_err());
        assert!(serde_json::from_value::<Question>(json!({"type": "choice"})).is_err());
        assert!(serde_json::from_value::<Question>(json!({"type": "noul", "extra": 1})).is_err());
    }

    #[test]
    fn validation() {
        let mut questions = BTreeMap::new();
        assert!(matches!(validate_questions(&questions), Err(Error::InvalidRequest(_))));

        questions.insert("s".into(), Question::from(Score::from_levels(Vec::<Value>::new())));
        let err = validate_questions(&questions).unwrap_err().to_string();
        assert!(err.contains("Score question \"s\" has no criteria"), "{err}");

        questions.clear();
        questions.insert("c".into(), Question::Custom(json!({"type": ""}).as_object().unwrap().clone()));
        assert!(validate_questions(&questions).unwrap_err().to_string().contains("nonempty string \"type\""));

        questions.clear();
        questions.insert("c".into(), Question::Custom(json!({"type": "choice"}).as_object().unwrap().clone()));
        assert!(validate_questions(&questions).unwrap_err().to_string().contains("requires \"criteria\""));

        questions.clear();
        questions.insert(
            "c".into(),
            Question::Custom(json!({"type": "score", "criteria": []}).as_object().unwrap().clone()),
        );
        assert!(validate_questions(&questions).unwrap_err().to_string().contains("no criteria"));

        questions.clear();
        questions.insert("ok".into(), Question::Custom(json!({"type": "future"}).as_object().unwrap().clone()));
        questions.insert("n".into(), Noul::default().into());
        questions.insert("ch".into(), Choice::new("x").label("a").into());
        questions.insert("sc".into(), Score::new("x", ["a"]).into());
        assert!(validate_questions(&questions).is_ok());
    }
}
