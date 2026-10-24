//! QR-code login ("remote auth"), the same flow discord.com's login page uses.
//!
//! 1. Connect to the remote-auth gateway and send it a fresh RSA public key.
//! 2. Prove we hold the private key by decrypting a nonce it sends.
//! 3. It gives us a fingerprint; we show `https://discord.com/ra/<fingerprint>`
//!    as a QR code.
//! 4. The user scans it in the Discord app, then confirms the login there.
//! 5. We trade the ticket we're given for the token, encrypted with our key.
//!
//! The password never touches this program, and nothing here is stored except
//! the final token (by the caller).

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use plume_core::Token;
use rsa::pkcs8::EncodePublicKey;
use rsa::sha2::{Digest, Sha256};
use rsa::{Oaep, RsaPrivateKey};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;
use tokio::time::{Instant, interval_at};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tracing::debug;

use crate::AuthError;

const REMOTE_AUTH_URL: &str = "wss://remote-auth-gateway.discord.gg/?v=2";
const TICKET_URL: &str = "https://discord.com/api/v9/users/@me/remote-auth/login";

/// Progress updates for the UI.
#[derive(Debug, Clone)]
pub enum QrStatus {
    /// Show this URL as a QR code.
    ShowQr { url: String },
    /// Someone scanned it and needs to confirm on their phone.
    Scanned { username: String },
}

/// Messages from the remote-auth gateway. Unlike the main gateway, `op` is a string.
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Incoming {
    Hello {
        heartbeat_interval: u64,
    },
    NonceProof {
        encrypted_nonce: String,
    },
    PendingRemoteInit {
        fingerprint: String,
    },
    PendingTicket {
        encrypted_user_payload: String,
    },
    PendingLogin {
        ticket: String,
    },
    HeartbeatAck,
    Cancel,
    #[serde(other)]
    Unknown,
}

/// Runs the whole QR login and returns the token. `on_status` is called when
/// there's something to show the user.
pub async fn qr_login(mut on_status: impl FnMut(QrStatus)) -> Result<Token, AuthError> {
    // The gateway refuses connections that don't look like they come from discord.com.
    let mut request = REMOTE_AUTH_URL.into_client_request()?;
    request
        .headers_mut()
        .insert("Origin", "https://discord.com".parse().expect("valid header"));
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await?;

    // Generating an RSA key takes real CPU time, so keep it off the async threads.
    let key = tokio::task::spawn_blocking(|| RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048))
        .await
        .map_err(|e| AuthError::Protocol(format!("key generation crashed: {e}")))??;

    // Placeholder until HELLO tells us the real interval.
    let mut ticker = interval_at(Instant::now() + Duration::from_secs(3600), Duration::from_secs(3600));

    loop {
        let message = tokio::select! {
            _ = ticker.tick() => {
                send(&mut socket, json!({ "op": "heartbeat" })).await?;
                continue;
            }
            message = socket.next() => message,
        };

        let text = match message {
            Some(Ok(Message::Text(text))) => text,
            Some(Ok(Message::Close(frame))) => {
                return Err(AuthError::Closed {
                    code: frame.as_ref().map(|f| u16::from(f.code)),
                    reason: frame.map(|f| f.reason.to_string()).unwrap_or_default(),
                });
            }
            Some(Ok(_)) => continue,
            Some(Err(error)) => return Err(error.into()),
            None => {
                return Err(AuthError::Closed {
                    code: None,
                    reason: "connection ended".to_owned(),
                });
            }
        };

        match serde_json::from_str::<Incoming>(&text)? {
            Incoming::Hello { heartbeat_interval } => {
                let every = Duration::from_millis(heartbeat_interval);
                ticker = interval_at(Instant::now() + every, every);
                let public_key = key
                    .to_public_key()
                    .to_public_key_der()
                    .map_err(|e| AuthError::Protocol(format!("couldn't encode public key: {e}")))?;
                send(
                    &mut socket,
                    json!({
                        "op": "init",
                        "encoded_public_key": STANDARD.encode(public_key.as_bytes()),
                    }),
                )
                .await?;
            }
            Incoming::NonceProof { encrypted_nonce } => {
                let nonce = decrypt(&key, &encrypted_nonce)?;
                let proof = URL_SAFE_NO_PAD.encode(Sha256::digest(&nonce));
                send(&mut socket, json!({ "op": "nonce_proof", "proof": proof })).await?;
            }
            Incoming::PendingRemoteInit { fingerprint } => {
                on_status(QrStatus::ShowQr {
                    url: format!("https://discord.com/ra/{fingerprint}"),
                });
            }
            Incoming::PendingTicket { encrypted_user_payload } => {
                // Decrypts to "id:discriminator:avatar:username".
                let payload = String::from_utf8_lossy(&decrypt(&key, &encrypted_user_payload)?).into_owned();
                let username = payload.splitn(4, ':').nth(3).unwrap_or("someone").to_owned();
                on_status(QrStatus::Scanned { username });
            }
            Incoming::PendingLogin { ticket } => {
                let encrypted_token = exchange_ticket(&ticket).await?;
                let token = String::from_utf8(decrypt(&key, &encrypted_token)?)
                    .map_err(|_| AuthError::Protocol("token is not valid UTF-8".to_owned()))?;
                let _ = socket.close(None).await;
                return Ok(Token::new(token));
            }
            Incoming::HeartbeatAck => debug!("remote auth heartbeat ACK"),
            Incoming::Cancel => return Err(AuthError::Cancelled),
            Incoming::Unknown => debug!("ignoring unknown remote auth message"),
        }
    }
}

/// Swaps the login ticket for the (encrypted) token over HTTPS.
async fn exchange_ticket(ticket: &str) -> Result<String, AuthError> {
    #[derive(Deserialize)]
    struct TicketResponse {
        encrypted_token: Option<String>,
        captcha_key: Option<serde_json::Value>,
        message: Option<String>,
    }

    let response = plume_rest::build_http_client()?
        .post(TICKET_URL)
        .json(&json!({ "ticket": ticket }))
        .send()
        .await?;
    let status = response.status();
    let body: TicketResponse = response.json().await?;

    if let Some(encrypted_token) = body.encrypted_token {
        return Ok(encrypted_token);
    }
    if body.captcha_key.is_some() {
        // We deliberately don't try to get around captchas.
        return Err(AuthError::CaptchaRequired);
    }
    Err(AuthError::Protocol(format!(
        "ticket exchange failed ({status}): {}",
        body.message.unwrap_or_default()
    )))
}

fn decrypt(key: &RsaPrivateKey, base64_data: &str) -> Result<Vec<u8>, AuthError> {
    let encrypted = STANDARD.decode(base64_data)?;
    Ok(key.decrypt(Oaep::new::<Sha256>(), &encrypted)?)
}

async fn send<S>(socket: &mut S, value: serde_json::Value) -> Result<(), AuthError>
where
    S: SinkExt<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    socket.send(Message::text(value.to_string())).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_remote_auth_messages() {
        let hello: Incoming =
            serde_json::from_str(r#"{"op":"hello","heartbeat_interval":41250,"timeout_ms":120000}"#).unwrap();
        assert!(matches!(
            hello,
            Incoming::Hello {
                heartbeat_interval: 41250
            }
        ));

        let nonce: Incoming = serde_json::from_str(r#"{"op":"nonce_proof","encrypted_nonce":"abc"}"#).unwrap();
        assert!(matches!(nonce, Incoming::NonceProof { .. }));

        let unknown: Incoming = serde_json::from_str(r#"{"op":"something_new"}"#).unwrap();
        assert!(matches!(unknown, Incoming::Unknown));
    }
}
