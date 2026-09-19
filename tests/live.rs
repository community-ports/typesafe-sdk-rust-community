//! Live API tests. They run only when `TYPESAFE_API_KEY` is set and are skipped otherwise, so
//! `cargo test` stays offline by default.

use typesafeai_sdk_community::{Choice, Noul, Score, TypeSafeClient, json};

fn live_client() -> Option<TypeSafeClient> {
    if std::env::var("TYPESAFE_API_KEY").ok().filter(|key| !key.trim().is_empty()).is_none() {
        eprintln!("skipping live test: TYPESAFE_API_KEY is not set");
        return None;
    }
    Some(TypeSafeClient::new().expect("client from environment"))
}

#[tokio::test]
async fn live_system_one_mixed_questions() {
    let Some(client) = live_client() else { return };
    let response = client
        .system_one(json!({"message": "I was charged twice for my subscription. Please fix this today."}))
        .question("billing", Noul::new("Is `message` about billing?"))
        .question("tone", Choice::new("What is the tone of `message`?").labels(["angry", "calm", "excited"]))
        .question("urgency", Score::new("How urgent is `message`?", ["Can wait", "This week", "Today"]))
        .send()
        .await
        .expect("live request succeeds");

    assert!(!response.model.is_empty());
    assert!(response.request_id().is_some());
    let billing = response.noul("billing").expect("noul answer");
    assert!((0.0..=1.0).contains(&billing.noul));
    let tone = response.choice("tone").expect("choice answer");
    assert!(["angry", "calm", "excited"].contains(&tone.choice.as_str()));
    let total: f64 = tone.probabilities.values().sum();
    assert!((total - 1.0).abs() < 0.05, "probabilities sum to {total}");
    let urgency = response.score("urgency").expect("score answer");
    assert!((0.0..=2.0).contains(&urgency.score));
    assert_eq!(urgency.legend.len(), 3);
}

#[tokio::test]
async fn live_models_list() {
    let Some(client) = live_client() else { return };
    let response = client.models().list().await.expect("live request succeeds");
    assert!(response.models.iter().any(|model| model.name == "jev-latest"), "{:?}", response.models);
}
