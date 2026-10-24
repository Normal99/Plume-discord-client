//! Getting a Discord token and keeping it safe.
//!
//! - [`qr_login`]: scan a QR code with the Discord phone app (preferred).
//! - [`token_from_env`] + [`verify_token`]: use a token the user pasted.
//! - [`keyring`]: store the token in the OS keyring (or, with no keyring, a
//!   file only you can read).

pub mod keyring;
mod qr_login;
mod token_login;

pub use qr_login::{QrStatus, qr_login};
pub use token_login::{token_from_env, verify_token};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("WebSocket error: {0}")]
    WebSocket(Box<tokio_tungstenite::tungstenite::Error>),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error(transparent)]
    Rest(#[from] plume_rest::RestError),

    #[error("invalid JSON: {0}")]
    Json(#[from] serde_json::Error),

    #[error("encryption error: {0}")]
    Rsa(#[from] rsa::Error),

    #[error("invalid base64 from Discord: {0}")]
    Base64(#[from] base64::DecodeError),

    #[error("keyring error: {0}")]
    Keyring(#[from] ::keyring::Error),

    #[error("couldn't use the token file: {0}")]
    TokenFile(std::io::Error),

    #[error("unexpected response from Discord: {0}")]
    Protocol(String),

    #[error("Discord wants a captcha for this login. Try again later, or log in another way")]
    CaptchaRequired,

    #[error("Discord didn't accept that token")]
    InvalidToken,

    #[error("the login was cancelled on the phone")]
    Cancelled,

    #[error("the QR code expired or the connection closed (code {code:?}): {reason}")]
    Closed { code: Option<u16>, reason: String },
}

impl From<tokio_tungstenite::tungstenite::Error> for AuthError {
    fn from(error: tokio_tungstenite::tungstenite::Error) -> Self {
        AuthError::WebSocket(Box::new(error))
    }
}
