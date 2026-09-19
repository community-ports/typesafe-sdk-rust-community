#![cfg(feature = "derive")]

mod support;

use serde_json::json;
use support::{MockServer, Reply, system_one_body};
use typesafeai_sdk_community::decision::{Decision, Gate, Outcome};
use typesafeai_sdk_community::typed::{ChoiceLabels as _, Questions as _, ScoreLevels as _};
use typesafeai_sdk_community::{
    Answer, AnswerError, Choice, ChoiceAnswer, ChoiceLabels, Error, Noul, NoulAnswer, Questions, RetryPolicy, Score,
    ScoreAnswer, ScoreLevels, TypeSafeClient, TypedChoice, TypedScore,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ChoiceLabels)]
enum Tone {
    #[choice(describe = "An upset or hostile message")]
    Angry,
    #[choice(describe = json!({"summary": "Neutral or polite", "examples": ["Thanks!", "Could you help?"]}))]
    Calm,
    Excited,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ChoiceLabels)]
#[choice(rename_all = "kebab-case")]
enum Route {
    BillingTeam,
    #[choice(label = "tech")]
    TechnicalSupport,
    NoMatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ScoreLevels)]
enum Urgency {
    #[score("Can wait")]
    Low,
    #[score(describe = "Needs attention this week")]
    Medium,
    NeedsAttentionToday,
}

#[derive(Debug, Questions)]
struct Triage {
    #[noul("Is this message about billing?", when_true = "Charges or refunds", when_false = "Anything else")]
    billing: NoulAnswer,
    #[choice("What is the tone of the message?")]
    tone: TypedChoice<Tone>,
    #[score("How urgent is the message?")]
    urgency: TypedScore<Urgency>,
}

#[derive(Debug, Questions)]
#[questions(crate = "typesafeai_sdk_community")]
struct Flexible {
    #[noul("Billing?", name = "billing")]
    is_billing: bool,
    #[choice("Tone?")]
    tone: Tone,
    #[score("Urgency?", name = "urgency", levels = ["Can wait", "This week", "Today"])]
    raw_urgency: ScoreAnswer,
    #[noul("Never answered")]
    absent: Option<NoulAnswer>,
}

#[derive(Debug, Questions)]
struct Views {
    #[noul(instructions = json!({"task": "Billing?"}), name = "billing")]
    billing_probability: f64,
    #[choice("Tone?", name = "tone", labels = ["angry", ("calm", "Polite"), "excited"])]
    raw_tone: ChoiceAnswer,
    #[score("Urgency?", name = "urgency", levels = ["Can wait", "This week", "Today"])]
    expected: f64,
}

#[derive(Debug, Questions)]
struct Loose {
    #[choice("Tone?", name = "tone")]
    labeled: Option<TypedChoice<Tone>>,
    #[score("Urgency?")]
    urgency: Urgency,
    #[noul("Billing?", name = "billing")]
    any: Answer,
}

fn client(server: &MockServer) -> TypeSafeClient {
    TypeSafeClient::builder().api_key("sk-test").base_url(server.url()).retry(RetryPolicy::none()).build().unwrap()
}

#[test]
fn derived_labels_and_levels() {
    assert_eq!(Tone::ALL, &[Tone::Angry, Tone::Calm, Tone::Excited]);
    assert_eq!(Tone::Angry.label(), "angry");
    assert_eq!(Tone::from_label("excited"), Some(Tone::Excited));
    assert_eq!(Tone::from_label("nope"), None);
    assert_eq!(Tone::Excited.describe(), None);
    assert_eq!(Tone::Calm.describe().unwrap()["summary"], json!("Neutral or polite"));
    assert_eq!(
        serde_json::to_value(Tone::criteria()).unwrap(),
        json!({
            "angry": "An upset or hostile message",
            "calm": {"summary": "Neutral or polite", "examples": ["Thanks!", "Could you help?"]},
            "excited": null
        })
    );

    assert_eq!(Route::BillingTeam.label(), "billing-team");
    assert_eq!(Route::TechnicalSupport.label(), "tech");
    assert_eq!(Route::NoMatch.label(), "no-match");

    assert_eq!(Urgency::Medium.level(), 1);
    assert_eq!(Urgency::from_level(2), Some(Urgency::NeedsAttentionToday));
    assert_eq!(Urgency::max_level(), 2);
    assert_eq!(
        Urgency::criteria(),
        vec![json!("Can wait"), json!("Needs attention this week"), json!("Needs attention today")]
    );

    let choice = Choice::of::<Route>("Which team?");
    assert_eq!(choice.criteria.len(), 3);
    let score = Score::of::<Urgency>("How urgent?");
    assert_eq!(score.criteria.len(), 3);
}

#[test]
fn derived_questions_wire_format() {
    let questions = Triage::questions();
    assert_eq!(
        serde_json::to_value(&questions).unwrap(),
        json!({
            "billing": {
                "type": "noul",
                "instructions": "Is this message about billing?",
                "criteria": {"true": "Charges or refunds", "false": "Anything else"}
            },
            "tone": {
                "type": "choice",
                "instructions": "What is the tone of the message?",
                "criteria": {
                    "angry": "An upset or hostile message",
                    "calm": {"summary": "Neutral or polite", "examples": ["Thanks!", "Could you help?"]},
                    "excited": null
                }
            },
            "urgency": {
                "type": "score",
                "instructions": "How urgent is the message?",
                "criteria": ["Can wait", "Needs attention this week", "Needs attention today"]
            }
        })
    );

    let flexible = serde_json::to_value(Flexible::questions()).unwrap();
    assert_eq!(flexible.as_object().unwrap().len(), 4);
    assert_eq!(flexible["tone"]["criteria"], serde_json::to_value(Tone::criteria()).unwrap());
    assert_eq!(flexible["urgency"]["criteria"], json!(["Can wait", "This week", "Today"]));
    assert_eq!(flexible["absent"], json!({"type": "noul", "instructions": "Never answered"}));

    let views = serde_json::to_value(Views::questions()).unwrap();
    assert_eq!(views["billing"]["instructions"], json!({"task": "Billing?"}));
    assert_eq!(views["tone"]["criteria"], json!({"angry": null, "calm": "Polite", "excited": null}));
    assert_eq!(views["urgency"]["criteria"], json!(["Can wait", "This week", "Today"]));
}

#[tokio::test]
async fn ask_sends_the_set_and_parses_typed_answers() {
    let server = MockServer::start();
    server.enqueue(Reply::json(200, system_one_body()).header("x-typesafe-request-id", "typed1"));

    let triage = client(&server).ask::<Triage>(json!({"message": "I was charged twice!"})).send_full().await.unwrap();
    assert_eq!(triage.response.request_id(), Some("typed1"));
    assert_eq!(triage.billing.noul, 0.98);
    assert!(triage.billing.decide(0.9));
    assert_eq!(triage.billing.decide_with(typesafeai_sdk_community::decision::Bands::new(0.3, 0.7)), Decision::Yes);
    assert_eq!(triage.tone.choice, Tone::Angry);
    assert_eq!(triage.tone.probabilities[0], (Tone::Angry, 0.8));
    assert_eq!(triage.tone.probability(Tone::Excited), 0.1);
    assert_eq!(triage.urgency.most_likely, Urgency::NeedsAttentionToday);
    assert_eq!(triage.urgency.score, 1.7);
    assert!((triage.urgency.probability_at_least(Urgency::Medium) - 0.9).abs() < 1e-9);
    assert_eq!(Gate::new(0.85, 0.6).evaluate(triage.tone.confidence), Outcome::Accept);

    let request = server.last_request().json();
    assert_eq!(request["state"], json!({"message": "I was charged twice!"}));
    assert_eq!(request["questions"], serde_json::to_value(Triage::questions()).unwrap());
}

#[tokio::test]
async fn ask_supports_every_field_shape_and_options() {
    let server = MockServer::start();
    server.fallback(Reply::json(200, system_one_body()));
    let client = client(&server);

    let flexible: Flexible = client
        .ask::<Flexible>("x")
        .question("extra", Noul::new("Extra?"))
        .model("jev-pinned")
        .header("x-typed", "1")
        .await
        .unwrap();
    assert!(flexible.is_billing);
    assert_eq!(flexible.tone, Tone::Angry);
    assert_eq!(flexible.raw_urgency.score, 1.7);
    assert!(flexible.absent.is_none());

    let request = server.last_request();
    assert_eq!(request.header("x-typed"), Some("1"));
    let body = request.json();
    assert_eq!(body["model"], json!("jev-pinned"));
    assert!(body["questions"].get("extra").is_some());

    let views = client.ask::<Views>("x").send().await.unwrap();
    assert_eq!(views.billing_probability, 0.98);
    assert_eq!(views.raw_tone.choice, "angry");
    assert_eq!(views.expected, 1.7);

    let loose = client.ask::<Loose>("x").send().await.unwrap();
    assert_eq!(loose.labeled.unwrap().choice, Tone::Angry);
    assert_eq!(loose.urgency, Urgency::NeedsAttentionToday);
    assert_eq!(loose.any.type_name(), "noul");
}

#[tokio::test]
async fn typed_parse_errors() {
    let server = MockServer::start();
    let client = client(&server);

    // Missing required question.
    server.enqueue(Reply::json(200, json!({"model": "m", "usage": {}, "answers": {"tone": {"type": "choice", "choice": "angry", "confidence": 1.0, "probabilities": {"angry": 1.0}}}})));
    let error = client.ask::<Triage>("x").send().await.unwrap_err();
    assert!(matches!(error, Error::Answer(AnswerError::Missing { ref name }) if name == "billing"), "{error}");
    assert_eq!(error.to_string(), "Answer \"billing\" is missing from the response.");

    // Unknown label.
    let mut body = system_one_body();
    body["answers"]["tone"]["choice"] = json!("sarcastic");
    server.enqueue(Reply::json(200, body));
    let error = client.ask::<Triage>("x").send().await.unwrap_err();
    assert!(matches!(error, Error::Answer(AnswerError::UnknownLabel { ref label, .. }) if label == "sarcastic"));

    // Wrong kind.
    let mut body = system_one_body();
    body["answers"]["billing"] = body["answers"]["tone"].clone();
    server.enqueue(Reply::json(200, body));
    let error = client.ask::<Triage>("x").send().await.unwrap_err();
    assert!(matches!(error, Error::Answer(AnswerError::WrongType { expected: "noul", actual: "choice", .. })));

    // Unknown level.
    let mut body = system_one_body();
    body["answers"]["urgency"]["probabilities"] = json!({"0": 0.5, "7": 0.5});
    server.enqueue(Reply::json(200, body));
    let error = client.ask::<Triage>("x").send().await.unwrap_err();
    assert!(matches!(error, Error::Answer(AnswerError::UnknownLevel { level: 7, .. })));
}

#[tokio::test]
async fn parse_and_typed_accessors_on_plain_responses() {
    let server = MockServer::start();
    server.enqueue(Reply::json(200, system_one_body()));
    let response = client(&server)
        .system_one("x")
        .question("tone", Choice::of::<Tone>("Tone?"))
        .question("urgency", Score::of::<Urgency>("Urgency?"))
        .send()
        .await
        .unwrap();
    let triage: Triage = response.parse().unwrap();
    assert_eq!(triage.tone.choice, Tone::Angry);
    assert_eq!(response.choice_as::<Tone>("tone").unwrap().choice, Tone::Angry);
    assert_eq!(response.score_as::<Urgency>("urgency").unwrap().most_likely, Urgency::NeedsAttentionToday);
    assert!(response.get::<bool>("billing").unwrap());
    assert_eq!(response.get::<Option<f64>>("nope").unwrap(), None);
    assert!(response.choice_as::<Tone>("billing").is_err());
}

#[cfg(feature = "blocking")]
#[test]
fn blocking_ask() {
    let server = MockServer::start();
    server.enqueue(Reply::json(200, system_one_body()));
    let client = typesafeai_sdk_community::blocking::TypeSafeClient::builder()
        .api_key("sk-test")
        .base_url(server.url())
        .retry(RetryPolicy::none())
        .build()
        .unwrap();
    let triage = client.ask::<Triage>("x").send().unwrap();
    assert_eq!(triage.tone.choice, Tone::Angry);
}
