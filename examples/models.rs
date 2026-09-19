//! List the models available to the account, with SDK logging enabled.
//!
//! ```sh
//! TYPESAFE_API_KEY=... RUST_LOG=typesafeai_sdk_community=debug cargo run --example models
//! ```

use tracing_subscriber::EnvFilter;
use typesafeai_sdk_community::TypeSafeClient;

#[tokio::main]
async fn main() -> typesafeai_sdk_community::Result<()> {
    tracing_subscriber::fmt().with_env_filter(EnvFilter::from_default_env()).init();

    let client = TypeSafeClient::new()?;
    let response = client.models().list().await?;
    for model in &response.models {
        println!("{:<16} {:<12} {}", model.name, model.release_date, model.description);
    }
    Ok(())
}
