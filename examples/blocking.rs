//! The same request as `basic`, from synchronous code.
//!
//! ```sh
//! TYPESAFE_API_KEY=... cargo run --example blocking --features blocking
//! ```

use typesafeai_sdk_community::blocking::TypeSafeClient;
use typesafeai_sdk_community::{Choice, Noul};

fn main() -> typesafeai_sdk_community::Result<()> {
    let client = TypeSafeClient::new()?;

    let response = client
        .system_one("I was charged twice. Please help.")
        .question("billing", Noul::new("Is this message about billing?"))
        .question("tone", Choice::new("What is the tone?").labels(["calm", "angry"]))
        .send()?;

    println!("billing: p(yes) = {:.3}", response.noul("billing").unwrap().noul);
    let tone = response.choice("tone").unwrap();
    println!("tone: {} (confidence {:.3})", tone.choice, tone.confidence);
    Ok(())
}
