//! Triage many tickets at once, rerank passages, and evaluate a question on labeled data.
//!
//! ```sh
//! TYPESAFE_API_KEY=... cargo run --example batch
//! ```

use std::time::Duration;

use typesafeai_sdk_community::cache::Cache;
use typesafeai_sdk_community::eval::Example;
use typesafeai_sdk_community::{ChoiceLabels, NoulAnswer, Questions, TypeSafeClient, TypedChoice, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq, ChoiceLabels)]
enum Team {
    Billing,
    Support,
    Sales,
    NoMatch,
}

#[derive(Debug, Questions)]
struct Triage {
    #[choice("Which team should handle `message`?")]
    team: TypedChoice<Team>,
    #[noul("Does `message` ask for a refund?")]
    refund: NoulAnswer,
}

#[tokio::main]
async fn main() -> typesafeai_sdk_community::Result<()> {
    // A cache means re-running this example costs nothing for unchanged tickets.
    let client = TypeSafeClient::builder()
        .cache(Cache::in_memory(1_000).ttl(Duration::from_secs(3600)))
        .check_paths(true)
        .build()?;

    let tickets = [
        "I was charged twice this month, refund one please",
        "The app crashes when I open settings",
        "Do you offer a discount for annual plans?",
        "Cancel my subscription and refund the last charge",
        "hello?",
    ];

    // Batch: five requests, up to four in flight, results in ticket order.
    let outcome = client
        .batch::<Triage>(tickets.iter().map(|t| json!({"message": t})))
        .concurrency(4)
        .check_paths()
        .on_progress(|p| eprintln!("  {}/{} done", p.completed, p.total))
        .run()
        .await;
    println!(
        "triage: {} ok, {} failed, {:?}, {} input tokens",
        outcome.succeeded(),
        outcome.failed(),
        outcome.elapsed,
        outcome.usage.input_tokens.unwrap_or(0)
    );
    for (index, triage) in outcome.ok() {
        println!("  [{index}] {:<8?} refund={:.2}  {}", triage.team.choice, triage.refund.noul, tickets[index]);
    }
    for (index, error) in outcome.errors() {
        println!("  [{index}] failed: {error}");
    }
    println!();

    // Rerank: one relevance question per passage, sorted.
    let passages = vec![
        "Refunds are issued to the original payment method within 5 business days.",
        "You can change your plan at any time from the billing page.",
        "Our support hours are 9am to 5pm on weekdays.",
        "Annual plans are billed once a year at a 20% discount.",
    ];
    let ranked = client.rerank("How long does a refund take?", passages).top(2).run().await?;
    println!("rerank:");
    for hit in &ranked {
        println!("  {:.2}  {}", hit.relevance, hit.candidate);
    }
    println!();

    // Find: every line in one request.
    let lines = vec![
        "1. Introduction",
        "2. You may cancel at any time from your account page.",
        "3. Fees are non-refundable after 30 days.",
    ];
    let found = client.find("When can I cancel?", lines.clone()).run().await?;
    match found.best(0.5) {
        Some(index) => println!("find: line {index} ({:.2} present): {}", found.present.noul, lines[index]),
        None => println!("find: nothing matches ({:.2} present)", found.present.noul),
    }
    println!();

    // Evaluate: how well does the refund question agree with labels, and which threshold to use?
    let examples = [
        ("Please refund me, I was double charged", true),
        ("Give me my money back now", true),
        ("How do I change my password?", false),
        ("Is there a student discount?", false),
        ("I want a chargeback for last month", true),
        ("Thanks, all sorted", false),
    ]
    .map(|(text, refund)| Example::new(json!({"message": text}), refund));
    let run = client.evaluate::<Triage, bool>(examples).concurrency(4).run().await;
    let report = run.binary(|triage| triage.refund.noul);
    println!("evaluation of `refund` on {} examples:\n{report}", report.n);
    println!("suggested threshold for best F1: {:.2}", report.best_f1.threshold);
    if let Some(cache) = client.cache() {
        println!("\ncache: {:?}", cache.stats());
    }
    Ok(())
}
