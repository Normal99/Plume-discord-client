//! Prints gateway events as they arrive. Log in first:
//!
//!     cargo run -p plume-auth --example login
//!     cargo run -p plume-gateway --example tail
//!
//! `RUST_LOG=plume_gateway=debug` shows the connection's inner workings.

use plume_core::Event;
use plume_gateway::Gateway;
use tokio::sync::mpsc;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let token = match plume_auth::token_from_env() {
        Some(token) => token,
        None => tokio::task::spawn_blocking(plume_auth::keyring::load_token)
            .await??
            .ok_or("not logged in: run `cargo run -p plume-auth --example login` first")?,
    };

    let (events_tx, mut events) = mpsc::channel(64);
    let gateway = tokio::spawn(Gateway::new(token, events_tx).run());

    while let Some(event) = events.recv().await {
        match event {
            Event::Ready {
                me,
                guilds,
                channels,
                users,
            } => println!(
                "READY as {}: {} servers, {} channels, {} users",
                me.display_name(),
                guilds.len(),
                channels.len(),
                users.len()
            ),
            Event::MessageCreate { message, author } => {
                println!(
                    "[{}] {}: {}",
                    message.channel_id,
                    author.display_name(),
                    message.content
                );
            }
            Event::Other(name) => println!("({name})"),
            other => println!("{other:?}"),
        }
    }
    gateway.await??;
    Ok(())
}
