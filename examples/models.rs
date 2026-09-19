//! List the models available to the account, with SDK logging enabled.
//!
//! ```sh
//! TYPESAFE_API_KEY=... RUST_LOG=typesafe_sdk=debug cargo run --example models
//! ```

use tracing_subscriber::EnvFilter;
use typesafe_sdk::TypeSafeClient;

#[tokio::main]
async fn main() -> typesafe_sdk::Result<()> {
    tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env()).init();

    let client = TypeSafeClient::new()?;
    let response = client.models().list().await?;
    for model in &response.models {
        println!("{:<16} {:<12} {}", model.name, model.release_date, model.description);
    }
    Ok(())
}
