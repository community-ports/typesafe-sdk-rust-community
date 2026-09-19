//! Typed questions: enums as labels and rubrics, a struct as the whole question set, and
//! decisions made from probabilities instead of strings.
//!
//! ```sh
//! TYPESAFE_API_KEY=... cargo run --example typed
//! ```

use typesafeai_sdk_community::decision::{Bands, Decision, Gate, Outcome};
use typesafeai_sdk_community::{
    ChoiceLabels, NoulAnswer, Questions, ScoreLevels, TypeSafeClient, TypedChoice, TypedScore, json,
};

/// Where a support message should go. The `NoMatch` label gives the model a way out when
/// nothing fits, which the docs recommend for every choice question.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ChoiceLabels)]
#[choice(rename_all = "kebab-case")]
enum Team {
    #[choice(describe = "Charges, invoices, refunds, or payment methods")]
    Billing,
    #[choice(describe = "Errors, outages, or how-to questions about the product")]
    TechnicalSupport,
    #[choice(describe = "Plans, upgrades, or pricing questions")]
    Sales,
    #[choice(describe = "None of the teams above clearly apply")]
    NoMatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ScoreLevels)]
enum Urgency {
    #[score("Can wait")]
    Low,
    #[score("Needs attention this week")]
    Medium,
    #[score("Needs attention today")]
    High,
}

#[derive(Debug, Questions)]
struct Triage {
    #[choice("Which team should handle `message`?")]
    team: TypedChoice<Team>,
    #[score("How urgent is `message`?")]
    urgency: TypedScore<Urgency>,
    #[noul(
        "Does `message` ask for a refund?",
        when_true = "The customer wants money back",
        when_false = "No refund requested"
    )]
    refund: NoulAnswer,
    #[noul("Is `message` written in a language other than English?")]
    non_english: bool,
}

#[tokio::main]
async fn main() -> typesafeai_sdk_community::Result<()> {
    let client = TypeSafeClient::new()?;

    let triage = client
        .ask::<Triage>(json!({
            "subject": "Duplicate charge",
            "message": "I was charged twice for my subscription this month. Please refund one and fix this today.",
        }))
        .send_full()
        .await?;

    println!("model: {}  request: {}", triage.response.model, triage.response.request_id().unwrap_or("-"));
    println!();

    // A typed choice: exhaustive `match`, no string comparisons.
    let team = &triage.team;
    let routing = match team.choice {
        Team::Billing => "billing queue",
        Team::TechnicalSupport => "support queue",
        Team::Sales => "sales inbox",
        Team::NoMatch => "manual triage",
    };
    println!("team: {:?} -> {routing} (confidence {:.2}, margin {:.2})", team.choice, team.confidence, team.margin());
    for (label, probability) in &team.probabilities {
        println!("  {label:<17?} {probability:.3}");
    }

    // Act, review, or reject based on confidence, with thresholds you own.
    match team.gate(Gate::new(0.85, 0.6)) {
        Outcome::Accept => println!("  -> route automatically"),
        Outcome::Review => println!("  -> route, but flag for a person to confirm"),
        Outcome::Reject => println!("  -> send to manual triage"),
    }
    println!();

    // A typed score: expected value, most likely level, and tail probabilities.
    let urgency = &triage.urgency;
    println!(
        "urgency: {:.2} ({:?}), P(at least Medium) = {:.2}",
        urgency.score,
        urgency.most_likely,
        urgency.probability_at_least(Urgency::Medium)
    );

    // A noul as a three-way decision instead of a bare threshold.
    match triage.refund.decide_with(Bands::new(0.3, 0.7)) {
        Decision::Yes => println!("refund: requested ({:.2})", triage.refund.noul),
        Decision::No => println!("refund: not requested ({:.2})", triage.refund.noul),
        Decision::Uncertain => println!("refund: unclear ({:.2}), ask the customer", triage.refund.noul),
    }
    println!("non-English: {}", triage.non_english);
    Ok(())
}
