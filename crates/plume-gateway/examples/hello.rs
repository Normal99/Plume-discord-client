//! Connects, waits for READY, prints a summary with memory use, and exits.
//! A quick check that login and READY parsing work for your account:
//!
//!     cargo run --release -p plume-gateway --example hello

use std::time::Instant;

use plume_core::{Event, Profile, Store};
use plume_gateway::Gateway;
use tokio::sync::mpsc;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let token = match plume_auth::token_from_env() {
        Some(token) => token,
        None => tokio::task::spawn_blocking(plume_auth::keyring::load_token)
            .await??
            .ok_or("not logged in: run `cargo run -p plume-auth --example login` first")?,
    };

    let start = Instant::now();
    let (events_tx, mut events) = mpsc::channel(64);
    let gateway = tokio::spawn(Gateway::new(token, events_tx).run());
    let mut store = Store::new(Profile::FEATHER);

    while let Some(event) = events.recv().await {
        if let Event::Ready { .. } = &event {
            store.apply(event);
            println!(
                "READY after {:.0?}: {} servers, {} channels; resident memory {}",
                start.elapsed(),
                store.guilds().count(),
                store.channels().count(),
                resident_memory().unwrap_or_else(|| "unknown".to_owned()),
            );
            break;
        }
    }
    drop(events);
    gateway.await??;
    Ok(())
}

fn resident_memory() -> Option<String> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
    Some(line.trim_start_matches("VmRSS:").trim().to_owned())
}
