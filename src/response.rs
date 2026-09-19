//! Answer objects and response metadata.
//!
//! [`SystemOneResponse`] holds answers keyed by the question names supplied in the request.
//! Each [`Answer`] is tagged by its `type`; the typed accessors ([`nouls`](SystemOneResponse::nouls),
//! [`choice`](SystemOneResponse::choice), and so on) do the matching for you.

use std::collections::BTreeMap;

use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::constants::REQUEST_ID_HEADER;
use crate::error::header_str;

/// A yes/no answer.
///
/// See the [noul primitive](https://docs.typesafe.ai/primitives/noul) for details.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoulAnswer {
    /// Probability of a yes answer or a true statement, from 0 to 1. Values near 1 favor yes,
    /// values near 0 favor no, and values near 0.5 indicate uncertainty.
    pub noul: f64,
}

/// A selected label and its probabilities.
///
/// See the [choice primitive](https://docs.typesafe.ai/primitives/choice) for details.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChoiceAnswer {
    /// The name of the choice with the highest probability among the question's criteria.
    pub choice: String,
    /// Confidence in the selected choice, from 0 to 1.
    pub confidence: f64,
    /// Probability of each choice in criteria, keyed by choice name; values sum to approximately 1.
    pub probabilities: BTreeMap<String, f64>,
}

/// An expected score with its rubric and probabilities.
///
/// See the [score primitive](https://docs.typesafe.ai/primitives/score) for details.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScoreAnswer {
    /// Expected score: the probability-weighted average of the rubric levels. May fall between
    /// integer levels.
    pub score: f64,
    /// Confidence in the score, from 0 to 1.
    pub confidence: f64,
    /// Rubric descriptions keyed by integer score.
    pub legend: BTreeMap<u32, Value>,
    /// Probabilities keyed by integer score; values sum to approximately 1.
    pub probabilities: BTreeMap<u32, f64>,
}

impl ScoreAnswer {
    /// The rubric level with the highest probability, if any.
    pub fn most_likely(&self) -> Option<u32> {
        self.probabilities.iter().max_by(|a, b| a.1.total_cmp(b.1)).map(|(level, _)| *level)
    }
}

/// An answer to a single question, identified by its `type`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
#[non_exhaustive]
pub enum Answer {
    /// A yes/no answer.
    Noul(NoulAnswer),
    /// A selected label.
    Choice(ChoiceAnswer),
    /// A rubric score.
    Score(ScoreAnswer),
}

impl Answer {
    /// The answer as a [`NoulAnswer`], if it is one.
    pub fn as_noul(&self) -> Option<&NoulAnswer> {
        match self {
            Answer::Noul(answer) => Some(answer),
            _ => None,
        }
    }

    /// The answer as a [`ChoiceAnswer`], if it is one.
    pub fn as_choice(&self) -> Option<&ChoiceAnswer> {
        match self {
            Answer::Choice(answer) => Some(answer),
            _ => None,
        }
    }

    /// The answer as a [`ScoreAnswer`], if it is one.
    pub fn as_score(&self) -> Option<&ScoreAnswer> {
        match self {
            Answer::Score(answer) => Some(answer),
            _ => None,
        }
    }
}

/// Token counts for a request, when reported by the API.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Number of input tokens used, or `None` when the API did not report it.
    #[serde(default)]
    pub input_tokens: Option<u64>,
    /// Number of output tokens used, or `None` when the API did not report it.
    #[serde(default)]
    pub output_tokens: Option<u64>,
}

/// HTTP metadata attached to every decoded response.
#[derive(Clone, Debug, Default)]
pub struct ResponseMeta {
    /// The HTTP status of the response.
    pub status: StatusCode,
    /// The HTTP response headers.
    pub headers: HeaderMap,
}

impl ResponseMeta {
    pub(crate) fn new(status: StatusCode, headers: HeaderMap) -> Self {
        ResponseMeta { status, headers }
    }

    /// The `x-typesafe-request-id` response header, or `None` if absent.
    pub fn request_id(&self) -> Option<&str> {
        header_str(&self.headers, REQUEST_ID_HEADER)
    }
}

/// Answers grouped by question type with model and usage metadata.
///
/// See [System One](https://docs.typesafe.ai/concepts/system-one) for details.
#[derive(Clone, Debug, Default)]
pub struct SystemOneResponse {
    /// The model used to answer the request.
    pub model: String,
    /// Token usage for the request.
    pub usage: Usage,
    /// All answer objects keyed by question name.
    pub answers: BTreeMap<String, Answer>,
    /// HTTP status and headers of the underlying response.
    pub meta: ResponseMeta,
}

impl SystemOneResponse {
    /// The `x-typesafe-request-id` response header, or `None` if absent.
    pub fn request_id(&self) -> Option<&str> {
        self.meta.request_id()
    }

    /// The answer to the named question, of any type.
    pub fn answer(&self, name: &str) -> Option<&Answer> {
        self.answers.get(name)
    }

    /// The yes/no answer to the named question, if it is one.
    pub fn noul(&self, name: &str) -> Option<&NoulAnswer> {
        self.answers.get(name).and_then(Answer::as_noul)
    }

    /// The choice answer to the named question, if it is one.
    pub fn choice(&self, name: &str) -> Option<&ChoiceAnswer> {
        self.answers.get(name).and_then(Answer::as_choice)
    }

    /// The score answer to the named question, if it is one.
    pub fn score(&self, name: &str) -> Option<&ScoreAnswer> {
        self.answers.get(name).and_then(Answer::as_score)
    }

    /// Yes/no answers keyed by question name.
    pub fn nouls(&self) -> impl Iterator<Item = (&str, &NoulAnswer)> {
        self.answers.iter().filter_map(|(name, answer)| Some((name.as_str(), answer.as_noul()?)))
    }

    /// Choice answers keyed by question name.
    pub fn choices(&self) -> impl Iterator<Item = (&str, &ChoiceAnswer)> {
        self.answers.iter().filter_map(|(name, answer)| Some((name.as_str(), answer.as_choice()?)))
    }

    /// Score answers keyed by question name.
    pub fn scores(&self) -> impl Iterator<Item = (&str, &ScoreAnswer)> {
        self.answers.iter().filter_map(|(name, answer)| Some((name.as_str(), answer.as_score()?)))
    }
}

/// Metadata describing a single available model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMetadata {
    /// Model name or alias accepted by a request's model field.
    pub name: String,
    /// Human-readable description of the model and its capabilities.
    pub description: String,
    /// Model release date, formatted as YYYY-MM-DD.
    pub release_date: String,
}

/// The models available to the account.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct ListModelsResponse {
    /// The available models.
    pub models: Vec<ModelMetadata>,
    /// HTTP status and headers of the underlying response.
    #[serde(skip)]
    pub meta: ResponseMeta,
}

impl ListModelsResponse {
    /// The `x-typesafe-request-id` response header, or `None` if absent.
    pub fn request_id(&self) -> Option<&str> {
        self.meta.request_id()
    }
}

/// A successful response left undecoded: status, headers, and raw body bytes.
#[derive(Clone, Debug, Default)]
pub struct RawResponse {
    /// HTTP status and headers of the response.
    pub meta: ResponseMeta,
    /// The raw response body.
    pub body: Vec<u8>,
}

impl RawResponse {
    /// The `x-typesafe-request-id` response header, or `None` if absent.
    pub fn request_id(&self) -> Option<&str> {
        self.meta.request_id()
    }

    /// Decode the body as JSON into any `serde` type.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> serde_json::Result<T> {
        serde_json::from_slice(&self.body)
    }

    /// The body as UTF-8 text, with invalid sequences replaced.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn score_answer_parses_integer_keys() {
        let answer: ScoreAnswer = serde_json::from_value(json!({
            "score": 1.7, "confidence": 0.9,
            "legend": {"0": "Can wait", "1": {"k": 1}, "2": ["x"]},
            "probabilities": {"0": 0.1, "1": 0.1, "2": 0.8}
        }))
        .unwrap();
        assert_eq!(answer.legend[&0], json!("Can wait"));
        assert_eq!(answer.probabilities[&2], 0.8);
        assert_eq!(answer.most_likely(), Some(2));
        assert!(
            serde_json::from_value::<ScoreAnswer>(json!({
                "score": 1.0, "confidence": 1.0, "legend": {"x": "bad"}, "probabilities": {}
            }))
            .is_err()
        );
    }

    #[test]
    fn answer_is_tagged_and_ignores_unknown_fields() {
        let answer: Answer = serde_json::from_value(json!({"type": "noul", "noul": 0.5, "future": 1})).unwrap();
        assert_eq!(answer, Answer::Noul(NoulAnswer { noul: 0.5 }));
        assert!(serde_json::from_value::<Answer>(json!({"type": "noul", "noul": "0.5"})).is_err());
        assert!(serde_json::from_value::<Answer>(json!({"type": "unknown"})).is_err());
        assert_eq!(serde_json::to_value(&answer).unwrap(), json!({"type": "noul", "noul": 0.5}));
    }

    #[test]
    fn typed_accessors() {
        let mut answers = BTreeMap::new();
        answers.insert("a".to_string(), Answer::Noul(NoulAnswer { noul: 0.2 }));
        answers.insert(
            "b".to_string(),
            Answer::Choice(ChoiceAnswer { choice: "x".into(), confidence: 1.0, probabilities: BTreeMap::new() }),
        );
        let response = SystemOneResponse { answers, ..Default::default() };
        assert_eq!(response.noul("a").unwrap().noul, 0.2);
        assert!(response.noul("b").is_none());
        assert!(response.choice("b").is_some());
        assert!(response.score("a").is_none());
        assert_eq!(response.nouls().count(), 1);
        assert_eq!(response.choices().count(), 1);
        assert_eq!(response.scores().count(), 0);
        assert!(response.request_id().is_none());
    }
}
