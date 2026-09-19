//! Check that the backticked paths questions refer to exist in the state being sent.
//!
//! The docs recommend pointing a question at a specific part of the state with a backticked
//! dot-and-index path, such as `` `ticket.messages[0].text` ``. When a field is renamed or a
//! state is built differently, those references silently point at nothing and the model judges
//! without the evidence. [`check`] finds them before the request is sent:
//!
//! ```
//! use typesafeai_sdk_community::state::check;
//! use typesafeai_sdk_community::{Noul, Question, json};
//! use std::collections::BTreeMap;
//!
//! let state = json!({"ticket": {"message": "I was charged twice"}});
//! let questions = BTreeMap::from([
//!     ("billing".to_string(), Question::from(Noul::new("Is `ticket.message` about billing?"))),
//!     ("tone".to_string(), Question::from(Noul::new("Is `ticket.body` angry?"))),
//! ]);
//! let issues = check(&state, &questions);
//! assert_eq!(issues.len(), 1);
//! assert_eq!(issues[0].question, "tone");
//! assert_eq!(issues[0].path, "ticket.body");
//! ```
//!
//! Enable it on a request with `.check_paths()` or for every request with
//! [`ClientBuilder::check_paths`](crate::ClientBuilder::check_paths); a failing check returns
//! [`Error::StatePath`](crate::Error::StatePath) without sending anything. The mock client
//! from the `testing` module enables it by default.
//!
//! A path is a run of identifiers joined by `.` with optional `[index]` segments. A path with
//! a `.` or `[` must resolve from the root of the state. A bare identifier must exist as a key
//! somewhere in the state, at any depth, so references to keys inside array items still pass.
//! Backticks around anything else (`` `en-US` ``, `` `POST /v1` ``) are ignored.
//!
//! Cost: a dotted path resolves directly; a bare identifier walks the whole state, so a request
//! with many bare references and a very large state pays a full traversal per reference. That
//! is negligible next to a request, but worth knowing when `check_paths` is on for every item
//! of a large batch.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::Value;

use crate::question::Question;

/// A backticked reference that does not exist in the state.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct PathIssue {
    /// The question that references the path.
    pub question: String,
    /// The path as written, without backticks.
    pub path: String,
}

impl fmt::Display for PathIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "question {:?} references `{}`, which is not in the state", self.question, self.path)
    }
}

/// One or more question references that the state does not contain.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct StatePathError {
    /// Every unresolved reference.
    pub issues: Vec<PathIssue>,
}

impl fmt::Display for StatePathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} question reference(s) not found in the state: ", self.issues.len())?;
        for (i, issue) in self.issues.iter().enumerate() {
            if i > 0 {
                write!(f, "; ")?;
            }
            write!(f, "{:?} -> `{}`", issue.question, issue.path)?;
        }
        Ok(())
    }
}

impl std::error::Error for StatePathError {}

/// Check every question's instructions and criteria against `state`.
pub fn check(state: &Value, questions: &BTreeMap<String, Question>) -> Vec<PathIssue> {
    let mut issues = Vec::new();
    for (name, question) in questions {
        let Ok(value) = serde_json::to_value(question) else { continue };
        let mut paths = Vec::new();
        collect_strings(&value, &mut |text| paths.extend(referenced_paths(text)));
        paths.sort();
        paths.dedup();
        for path in paths {
            if !resolves(state, &path) {
                issues.push(PathIssue { question: name.clone(), path });
            }
        }
    }
    issues
}

/// The backticked path-shaped references in a piece of text.
pub fn referenced_paths(text: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('`') else { break };
        let candidate = &after[..end];
        if is_path(candidate) {
            paths.push(candidate.to_string());
        }
        rest = &after[end + 1..];
    }
    paths
}

/// `ident(.ident|[digits])*`
fn is_path(text: &str) -> bool {
    if text.is_empty() || text.len() > 200 {
        return false;
    }
    let mut chars = text.chars().peekable();
    let ident = |chars: &mut std::iter::Peekable<std::str::Chars<'_>>| -> bool {
        let mut any = false;
        while let Some(&c) = chars.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                chars.next();
                any = true;
            } else {
                break;
            }
        }
        any
    };
    if !ident(&mut chars) {
        return false;
    }
    loop {
        match chars.next() {
            None => return true,
            Some('.') => {
                if !ident(&mut chars) {
                    return false;
                }
            }
            Some('[') => {
                let mut digits = false;
                while let Some(&c) = chars.peek() {
                    if c.is_ascii_digit() {
                        chars.next();
                        digits = true;
                    } else {
                        break;
                    }
                }
                if !digits || chars.next() != Some(']') {
                    return false;
                }
            }
            Some(_) => return false,
        }
    }
}

/// Whether `path` exists in `state` under the rules in the module docs.
pub fn resolves(state: &Value, path: &str) -> bool {
    if path.contains('.') || path.contains('[') {
        return resolve(state, path).is_some();
    }
    has_key_anywhere(state, path)
}

/// Resolve a dotted, indexed path from the root.
pub fn resolve<'a>(state: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = state;
    for segment in segments(path) {
        current = match segment {
            Segment::Key(key) => current.get(key)?,
            Segment::Index(index) => current.get(index)?,
        };
    }
    Some(current)
}

enum Segment<'a> {
    Key(&'a str),
    Index(usize),
}

fn segments(path: &str) -> Vec<Segment<'_>> {
    let mut out = Vec::new();
    for part in path.split('.') {
        let mut rest = part;
        if let Some(bracket) = rest.find('[') {
            if bracket > 0 {
                out.push(Segment::Key(&rest[..bracket]));
            }
            rest = &rest[bracket..];
            while let Some(stripped) = rest.strip_prefix('[') {
                let Some(end) = stripped.find(']') else { break };
                if let Ok(index) = stripped[..end].parse() {
                    out.push(Segment::Index(index));
                }
                rest = &stripped[end + 1..];
            }
        } else if !rest.is_empty() {
            out.push(Segment::Key(rest));
        }
    }
    out
}

fn has_key_anywhere(value: &Value, key: &str) -> bool {
    match value {
        Value::Object(map) => map.contains_key(key) || map.values().any(|v| has_key_anywhere(v, key)),
        Value::Array(items) => items.iter().any(|v| has_key_anywhere(v, key)),
        _ => false,
    }
}

fn collect_strings(value: &Value, f: &mut impl FnMut(&str)) {
    match value {
        Value::String(text) => f(text),
        Value::Array(items) => items.iter().for_each(|v| collect_strings(v, f)),
        Value::Object(map) => map.iter().for_each(|(key, v)| {
            f(key);
            collect_strings(v, f);
        }),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::question::{Choice, Noul};
    use serde_json::json;

    #[test]
    fn path_syntax() {
        for ok in ["message", "ticket.message", "items[0]", "ticket.messages[0].text", "a_b.c2", "x[10][2]"] {
            assert!(is_path(ok), "{ok}");
        }
        for bad in ["", "en-US", "POST /v1", "a..b", "a[]", "a[x]", ".a", "a.", "-a", "a b", "a-b"] {
            assert!(!is_path(bad), "{bad}");
        }
        assert_eq!(
            referenced_paths("Is `ticket.message` about `billing` or `POST /v1`?"),
            ["ticket.message", "billing"]
        );
        assert_eq!(referenced_paths("no backticks"), Vec::<String>::new());
        assert_eq!(referenced_paths("unterminated `x"), Vec::<String>::new());
    }

    #[test]
    fn resolution_rules() {
        let state = json!({"ticket": {"messages": [{"text": "hi", "id": 1}]}, "flag": true});
        assert!(resolves(&state, "ticket"));
        assert!(resolves(&state, "ticket.messages"));
        assert!(resolves(&state, "ticket.messages[0].text"));
        assert!(!resolves(&state, "ticket.messages[1].text"));
        assert!(!resolves(&state, "ticket.body"));
        assert!(resolves(&state, "id")); // bare identifier, found at depth
        assert!(!resolves(&state, "nope"));
        assert!(!resolves(&json!("plain string state"), "message"));
        assert_eq!(resolve(&state, "flag"), Some(&json!(true)));
    }

    #[test]
    fn check_reports_each_question_once_per_path() {
        let state = json!({"message": "x", "meta": {"lang": "en"}});
        let questions = BTreeMap::from([
            ("ok".to_string(), Question::from(Noul::new("Is `message` in `meta.lang`?"))),
            (
                "bad".to_string(),
                Question::from(
                    Choice::new("Tone of `body`?")
                        .option("angry", "`body` is hostile")
                        .option("calm", "`meta.tone` is neutral"),
                ),
            ),
        ]);
        let issues = check(&state, &questions);
        assert_eq!(
            issues,
            vec![
                PathIssue { question: "bad".into(), path: "body".into() },
                PathIssue { question: "bad".into(), path: "meta.tone".into() },
            ]
        );
        let error = StatePathError { issues };
        assert!(error.to_string().starts_with("2 question reference(s) not found in the state: \"bad\" -> `body`; "));
    }
}
