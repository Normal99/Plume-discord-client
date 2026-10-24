//! `plume-tui login` / `logout`. Runs in the normal terminal, before the UI.

use plume_auth::{AuthError, QrStatus, keyring, qr_login, token_from_env, verify_token};
use qrcode::QrCode;
use qrcode::render::unicode::Dense1x2;

pub async fn logout() -> Result<(), Box<dyn std::error::Error>> {
    tokio::task::spawn_blocking(keyring::delete_token).await??;
    println!("Logged out: removed the saved token from the keyring.");
    Ok(())
}

pub async fn login(use_token: bool) -> Result<(), Box<dyn std::error::Error>> {
    let token = if use_token {
        token_from_env().ok_or("DISCORD_TOKEN is not set. Run: read -rs DISCORD_TOKEN && export DISCORD_TOKEN")?
    } else {
        println!("Setting up QR login...");
        let result = qr_login(|status| match status {
            QrStatus::ShowQr { url } => show_qr(&url),
            QrStatus::Scanned { username } => {
                println!("Scanned by {username}. Confirm the login on your phone...");
            }
        })
        .await;
        match result {
            Ok(token) => token,
            Err(AuthError::CaptchaRequired) => {
                return Err(
                    "Discord wants a captcha for QR logins right now, and Plume doesn't solve \
                     those. Use `plume-tui login --token` instead (see the README)."
                        .into(),
                );
            }
            Err(error) => return Err(error.into()),
        }
    };

    let username = verify_token(&token).await.map_err(|e| e.to_string())?;
    tokio::task::spawn_blocking(move || keyring::save_token(&token)).await??;
    println!("Logged in as {username}! Start the client with: plume-tui");
    Ok(())
}

fn show_qr(url: &str) {
    match QrCode::new(url) {
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
