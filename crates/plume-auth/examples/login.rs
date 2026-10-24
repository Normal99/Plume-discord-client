//! Log in and save the token in the system keyring.
//!
//!     cargo run -p plume-auth --example login               # scan a QR code
//!     cargo run -p plume-auth --example login -- --token    # use $DISCORD_TOKEN
//!     cargo run -p plume-auth --example login -- --logout   # forget the token

use plume_auth::{AuthError, QrStatus, keyring, qr_login, token_from_env, verify_token};
use plume_core::Token;
use qrcode::QrCode;
use qrcode::render::unicode::Dense1x2;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let has_flag = |flag: &str| std::env::args().any(|arg| arg == flag);

    if has_flag("--logout") {
        tokio::task::spawn_blocking(keyring::delete_token).await??;
        println!("Logged out: removed the saved token from the keyring.");
        return Ok(());
    }

    if tokio::task::spawn_blocking(keyring::load_token).await??.is_some() {
        println!("Already logged in (token is in the keyring).");
        println!("Run with `-- --logout` to remove it.");
        return Ok(());
    }

    let token = if has_flag("--token") {
        token_from_env().ok_or("DISCORD_TOKEN is not set. Run: read -rs DISCORD_TOKEN && export DISCORD_TOKEN")?
    } else {
        match login_with_qr().await {
            Ok(token) => token,
            Err(AuthError::CaptchaRequired) => {
                eprintln!("Discord wants a captcha for QR logins right now, and Plume doesn't solve those.");
                eprintln!("Use the token method instead (see the README), then run:");
                eprintln!("  cargo run -p plume-auth --example login -- --token");
                std::process::exit(1);
            }
            Err(error) => return Err(error.into()),
        }
    };

    // Check the token works before saving it.
    let username = verify_token(&token).await.map_err(|e| e.to_string())?;
    let storage = tokio::task::spawn_blocking(move || keyring::save_token(&token)).await??;
    println!("Logged in as {username}! Token saved: {storage:?}");
    println!("Next: cargo run -p plume-gateway --example tail");
    Ok(())
}

async fn login_with_qr() -> Result<Token, AuthError> {
    println!("Setting up QR login...");
    qr_login(|status| match status {
        QrStatus::ShowQr { url } => {
            match QrCode::new(&url) {
                // Swapped colours so it scans on a dark terminal background.
                Ok(code) => println!(
                    "\n{}",
                    code.render::<Dense1x2>()
                        .dark_color(Dense1x2::Light)
                        .light_color(Dense1x2::Dark)
                        .build()
                ),
                Err(error) => eprintln!("couldn't draw QR code: {error}"),
            }
            println!("Scan this with the Discord app: Settings -> Scan QR Code.");
            println!("Can't scan it? Make the terminal bigger, or zoom out.\n");
        }
        QrStatus::Scanned { username } => {
            println!("Scanned by {username}. Confirm the login on your phone...");
        }
    })
    .await
}
