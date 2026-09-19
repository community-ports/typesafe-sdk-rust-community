//! Typed routing (function calling) and composite scoring in one request each.
//!
//! ```sh
//! TYPESAFE_API_KEY=... cargo run --example route
//! ```

use typesafeai_sdk_community::composite::Composite as _;
use typesafeai_sdk_community::decision::{Gate, Outcome};
use typesafeai_sdk_community::typed::Route as _;
use typesafeai_sdk_community::{
    Composite, NoulAnswer, Questions, Route, ScoreLevels, TypeSafeClient, TypedScore, json,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ScoreLevels)]
enum Anger {
    #[score("Calm or neutral")]
    Calm,
    #[score("Irritated or impatient")]
    Irritated,
    #[score("Furious, threatening, or abusive")]
    Furious,
}

/// One request asks the routing question plus every branch's questions; only the winning
/// variant is built. Unused branches cost tokens but no extra round trips.
#[derive(Debug, Route)]
#[route("What does the customer want from `message`?")]
enum Intent {
    #[route(describe = "Wants money back for a charge")]
    Refund {
        #[noul("Does `message` ask for the full amount back, rather than a partial refund?")]
        full_amount: bool,
        #[score("How angry is the customer in `message`?")]
        anger: TypedScore<Anger>,
    },
    #[route(describe = "Wants to cancel or end the subscription")]
    Cancel {
        #[noul("Is the cancellation a threat to get something, rather than a decision?")]
        threat: NoulAnswer,
    },
    #[route(describe = "Asks how to do something or reports a problem")]
    Support,
    #[route(describe = "None of the above clearly applies")]
    Other,
}

/// Independent risk signals scored once, combined with weights code owns.
#[derive(Debug, Questions, Composite)]
struct ChurnRisk {
    #[noul("Does `message` express intent to leave?")]
    #[weight(0.5)]
    leaving: NoulAnswer,
    #[noul("Does `message` mention a competitor?")]
    #[weight(0.2)]
    competitor: NoulAnswer,
    #[score("How angry is the customer in `message`?")]
    #[weight(0.3)]
    anger: TypedScore<Anger>,
}

#[tokio::main]
async fn main() -> typesafeai_sdk_community::Result<()> {
    let client = TypeSafeClient::new()?;
    let state = json!({
        "message": "I was charged twice for my subscription this month. Refund both charges today or I'm cancelling and moving to your competitor.",
    });

    let routed = client.route::<Intent>(state.clone()).send_full().await?;
    println!(
        "route: {} (confidence {:.2}, margin {:.2})",
        routed.route.label(),
        routed.choice.confidence,
        routed.margin()
    );
    for (label, probability) in routed.choice.ranked() {
        println!("  {label:<10} {probability:.3}");
    }
    match &routed.route {
        Intent::Refund { full_amount, anger } => {
            println!("-> refund: full_amount={full_amount}, anger={:?} ({:.2})", anger.most_likely, anger.score);
        }
        Intent::Cancel { threat } => println!("-> cancel: threat={:.2}", threat.noul),
        Intent::Support => println!("-> support"),
        Intent::Other => println!("-> other"),
    }
    match routed.choice.gate(Gate::new(0.85, 0.6)) {
        Outcome::Accept => println!("   act automatically"),
        Outcome::Review => println!("   act, but flag for review"),
        Outcome::Reject => println!("   route to a person"),
    }
    println!();

    let risk = client.ask::<ChurnRisk>(state).send().await?;
    println!("churn risk: {:.2}", risk.composite());
    for part in risk.breakdown() {
        println!(
            "  {:<11} signal={:.2} weight={:.2} contributes={:.3}",
            part.name,
            part.signal.unwrap_or(0.0),
            part.weight,
            part.contribution
        );
    }
    Ok(())
}
