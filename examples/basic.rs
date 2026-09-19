//! Ask a few mixed questions about a support message.
//!
//! ```sh
//! TYPESAFE_API_KEY=... cargo run --example basic
//! ```

use typesafe_sdk::{Choice, Noul, Score, TypeSafeClient, json};

#[tokio::main]
async fn main() -> typesafe_sdk::Result<()> {
    let client = TypeSafeClient::new()?;

    let response = client
        .system_one(json!({
            "subject": "Duplicate charge",
            "message": "I was charged twice for my subscription this month. Please fix this today.",
        }))
        .question(
            "billing",
            Noul::new("Is this message about billing?")
                .when_true("The customer mentions charges, invoices, payments, or refunds.")
                .when_false("The message is about something other than money."),
        )
        .question(
            "tone",
            Choice::new("What is the tone of `message`?")
                .option("angry", "An upset or hostile message")
                .option("calm", "A neutral or polite message")
                .option("excited", "An enthusiastic or eager message"),
        )
        .question(
            "urgency",
            Score::new(
                "How urgent is this message?",
                ["Can wait", "Needs attention this week", "Needs attention today"],
            ),
        )
        .send()
        .await?;

    println!("model: {}", response.model);
    println!("request id: {}", response.request_id().unwrap_or("-"));
    println!("usage: {:?}", response.usage);
    println!();

    let billing = response.noul("billing").expect("billing is a noul answer");
    println!("billing: p(yes) = {:.3}", billing.noul);

    let tone = response.choice("tone").expect("tone is a choice answer");
    println!("tone: {} (confidence {:.3})", tone.choice, tone.confidence);
    for (label, probability) in &tone.probabilities {
        println!("  {label:<8} {probability:.3}");
    }

    let urgency = response.score("urgency").expect("urgency is a score answer");
    println!("urgency: {:.2} (confidence {:.3})", urgency.score, urgency.confidence);
    for (level, probability) in &urgency.probabilities {
        println!("  {level} {:<26} {probability:.3}", urgency.legend[level]);
    }
    Ok(())
}
