use thiserror::Error;

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("Discord rejected the token")]
    AuthenticationFailed,

    #[error("Discord closed the connection for good (code {code}): {reason}")]
    Fatal { code: u16, reason: String },

    #[error("WebSocket error: {0}")]
    WebSocket(Box<tokio_tungstenite::tungstenite::Error>),

    #[error("invalid JSON from the gateway: {0}")]
    Json(#[from] serde_json::Error),

    #[error("couldn't decompress a gateway message: {0}")]
    Decompress(#[from] flate2::DecompressError),

    #[error("the gateway broke protocol: {0}")]
    Protocol(String),
}

impl From<tokio_tungstenite::tungstenite::Error> for GatewayError {
    fn from(error: tokio_tungstenite::tungstenite::Error) -> Self {
        GatewayError::WebSocket(Box::new(error))
    }
}
