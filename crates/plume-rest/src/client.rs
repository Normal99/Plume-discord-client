use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use plume_core::identity::USER_AGENT;
use plume_core::models::{Message, User};
use plume_core::wire::{WireMessage, WireUser};
use plume_core::{Snowflake, Token};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use reqwest::{Response, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tracing::{debug, warn};

use crate::errors::RestError;
use crate::ratelimit::RateLimiter;
use crate::routes::{Route, encode_emoji};

const API_BASE: &str = "https://discord.com/api/v9";

const MAX_ATTEMPTS: u32 = 3;

/// The largest file a free account can upload.
pub const MAX_UPLOAD_BYTES: usize = 10 * 1024 * 1024;

/// What a request carries.
enum Body {
    Json(serde_json::Value),
    /// A ready-made `multipart/form-data` body and its boundary.
    Multipart {
        boundary: String,
        bytes: Bytes,
    },
}

/// An HTTP client with our TLS setup and user agent, for any Discord request.
pub fn build_http_client() -> reqwest::Result<reqwest::Client> {
    // reqwest is built without a default crypto backend, so pick `ring`
    // (the same one the WebSocket side uses). An error only means one is
    // already installed, which is fine.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder().user_agent(USER_AGENT).build()
}

/// Talks to Discord's REST API as the logged-in user. Cheap to clone: all
/// clones share one connection pool and one rate limiter.
#[derive(Clone)]
pub struct RestClient {
    inner: Arc<Inner>,
}

struct Inner {
    http: reqwest::Client,
    auth: HeaderValue,
    limiter: RateLimiter,
}

impl RestClient {
    pub fn new(token: &Token) -> Result<Self, RestError> {
        let mut auth = HeaderValue::from_str(token.expose()).map_err(|_| RestError::Unauthorized)?;
        // Keeps the token out of reqwest's debug output.
        auth.set_sensitive(true);
        Ok(RestClient {
            inner: Arc::new(Inner {
                http: build_http_client()?,
                auth,
                limiter: RateLimiter::default(),
            }),
        })
    }

    /// Who the token belongs to.
    pub async fn current_user(&self) -> Result<User, RestError> {
        let user: WireUser = self.request(Route::CurrentUser, None).await?;
        Ok(user.into_model())
    }

    /// The newest `limit` (max 100) messages in a channel, oldest first.
    pub async fn messages(&self, channel_id: Snowflake, limit: u8) -> Result<Vec<(Message, User)>, RestError> {
        self.messages_before(channel_id, None, limit).await
    }

    /// Up to `limit` (max 100) messages older than `before` (or the newest,
    /// if `None`), oldest first.
    pub async fn messages_before(
        &self,
        channel_id: Snowflake,
        before: Option<Snowflake>,
        limit: u8,
    ) -> Result<Vec<(Message, User)>, RestError> {
        let route = Route::GetMessages { channel_id };
        let mut query = format!("?limit={}", limit.clamp(1, 100));
        if let Some(before) = before {
            query.push_str(&format!("&before={before}"));
        }
        let wire: Vec<WireMessage> = self.request_with_query(route, &query, None).await?;
        // Discord sends newest first.
        Ok(wire.into_iter().rev().map(WireMessage::into_models).collect())
    }

    /// Sends a message, optionally as a reply. Returns it as Discord stored it.
    pub async fn send_message(
        &self,
        channel_id: Snowflake,
        content: &str,
        reply_to: Option<Snowflake>,
    ) -> Result<(Message, User), RestError> {
        let mut body = json!({
            "content": content,
            // A nonce lets Discord drop accidental duplicates.
            "nonce": now_snowflake().to_string(),
            "tts": false,
        });
        if let Some(message_id) = reply_to {
            body["message_reference"] = json!({ "message_id": message_id });
        }
        let wire: WireMessage = self.request(Route::CreateMessage { channel_id }, Some(body)).await?;
        Ok(wire.into_models())
    }

    /// Sends a file, with optional text, optionally as a reply.
    pub async fn upload(
        &self,
        channel_id: Snowflake,
        filename: &str,
        data: Vec<u8>,
        content: &str,
        reply_to: Option<Snowflake>,
    ) -> Result<(Message, User), RestError> {
        let mut payload = json!({
            "content": content,
            "nonce": now_snowflake().to_string(),
            "attachments": [{ "id": "0", "filename": filename }],
        });
        if let Some(message_id) = reply_to {
            payload["message_reference"] = json!({ "message_id": message_id });
        }
        let boundary = format!("plume{}", now_snowflake());
        let bytes = multipart(&boundary, &payload.to_string(), filename, &data);
        let response = self
            .send(
                Route::CreateMessage { channel_id },
                "",
                Some(Body::Multipart { boundary, bytes }),
            )
            .await?;
        let wire: WireMessage = serde_json::from_slice(&response.bytes().await?)?;
        Ok(wire.into_models())
    }

    /// Changes the text of one of your messages.
    pub async fn edit_message(
        &self,
        channel_id: Snowflake,
        message_id: Snowflake,
        content: &str,
    ) -> Result<(Message, User), RestError> {
        let route = Route::EditMessage { channel_id, message_id };
        let wire: WireMessage = self.request(route, Some(json!({ "content": content }))).await?;
        Ok(wire.into_models())
    }

    pub async fn delete_message(&self, channel_id: Snowflake, message_id: Snowflake) -> Result<(), RestError> {
        self.send(Route::DeleteMessage { channel_id, message_id }, "", None)
            .await?;
        Ok(())
    }

    /// Marks the channel read up to `message_id`, so other devices agree.
    pub async fn ack(&self, channel_id: Snowflake, message_id: Snowflake) -> Result<(), RestError> {
        let body = json!({ "token": null });
        self.send(Route::Ack { channel_id, message_id }, "", Some(Body::Json(body)))
            .await?;
        Ok(())
    }

    /// Adds (or with `add: false`, removes) your reaction. `id` is set for
    /// custom emoji.
    pub async fn react(
        &self,
        channel_id: Snowflake,
        message_id: Snowflake,
        name: &str,
        id: Option<Snowflake>,
        add: bool,
    ) -> Result<(), RestError> {
        let emoji = encode_emoji(name, id);
        let route = if add {
            Route::AddReaction {
                channel_id,
                message_id,
                emoji,
            }
        } else {
            Route::RemoveReaction {
                channel_id,
                message_id,
                emoji,
            }
        };
        self.send(route, "", None).await?;
        Ok(())
    }

    /// Shows "… is typing" to others for ~10 seconds.
    pub async fn trigger_typing(&self, channel_id: Snowflake) -> Result<(), RestError> {
        self.send(Route::TriggerTyping { channel_id }, "", None).await?;
        Ok(())
    }

    /// Rings everyone else in a DM call you've started.
    pub async fn ring(&self, channel_id: Snowflake) -> Result<(), RestError> {
        let body = json!({ "recipients": null });
        self.send(Route::RingCall { channel_id }, "", Some(Body::Json(body)))
            .await?;
        Ok(())
    }

    async fn request<T: DeserializeOwned>(
        &self,
        route: Route,
        body: Option<serde_json::Value>,
    ) -> Result<T, RestError> {
        self.request_with_query(route, "", body).await
    }

    async fn request_with_query<T: DeserializeOwned>(
        &self,
        route: Route,
        query: &str,
        body: Option<serde_json::Value>,
    ) -> Result<T, RestError> {
        let response = self.send(route, query, body.map(Body::Json)).await?;
        let bytes = response.bytes().await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Sends a request through the rate limiter, retrying on 429.
    async fn send(&self, route: Route, query: &str, body: Option<Body>) -> Result<Response, RestError> {
        let key = route.bucket_key();
        let path = route.path();
        let method = route.method();
        let url = format!("{API_BASE}{path}{query}");

        for _ in 0..MAX_ATTEMPTS {
            self.inner.limiter.acquire(&key).await;

            let mut request = self
                .inner
                .http
                .request(method.clone(), &url)
                .header(AUTHORIZATION, self.inner.auth.clone());
            match &body {
                Some(Body::Json(json)) => request = request.json(json),
                Some(Body::Multipart { boundary, bytes }) => {
                    request = request
                        .header(CONTENT_TYPE, format!("multipart/form-data; boundary={boundary}"))
                        .body(bytes.clone()); // cheap: `Bytes` is reference counted
                }
                None => {}
            }
            debug!("{method} {path}");
            let response = request.send().await?;
            self.inner.limiter.update(&key, response.headers());

            match response.status() {
                status if status.is_success() => return Ok(response),
                StatusCode::TOO_MANY_REQUESTS => {
                    let limit: TooManyRequests = response.json().await.unwrap_or_default();
                    let wait = Duration::from_secs_f64(limit.retry_after.max(0.5));
                    warn!("rate limited on {key} (global: {}); waiting {wait:.1?}", limit.global);
                    self.inner.limiter.penalize(&key, wait, limit.global);
                }
                StatusCode::UNAUTHORIZED => return Err(RestError::Unauthorized),
                StatusCode::FORBIDDEN => return Err(RestError::Forbidden),
                StatusCode::NOT_FOUND => return Err(RestError::NotFound),
                status => {
                    let error: ApiError = response.json().await.unwrap_or_default();
                    return Err(RestError::Api {
                        status,
                        message: error.message,
                    });
                }
            }
        }
        Err(RestError::RateLimited)
    }
}

#[derive(Default, Deserialize)]
struct TooManyRequests {
    #[serde(default)]
    retry_after: f64,
    #[serde(default)]
    global: bool,
}

#[derive(Default, Deserialize)]
struct ApiError {
    #[serde(default)]
    message: String,
}

/// A `multipart/form-data` body with the JSON payload and one file.
fn multipart(boundary: &str, payload: &str, filename: &str, data: &[u8]) -> Bytes {
    // Quotes and line breaks would break out of the header.
    let filename: String = filename
        .chars()
        .map(|c| if c == '"' || c.is_control() { '_' } else { c })
        .collect();
    let mut body = Vec::with_capacity(data.len() + payload.len() + 512);
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"payload_json\"\r\n\
             Content-Type: application/json\r\n\r\n{payload}\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"files[0]\"; filename=\"{filename}\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(data);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Bytes::from(body)
}

fn now_snowflake() -> Snowflake {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Snowflake::from_timestamp_ms(now_ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multipart_body_has_both_parts_and_a_safe_filename() {
        let body = multipart("XYZ", r#"{"content":"hi"}"#, "evil\"\r\nname.txt", b"\x00data");
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with("--XYZ\r\nContent-Disposition: form-data; name=\"payload_json\"\r\n"));
        assert!(text.contains("\r\n\r\n{\"content\":\"hi\"}\r\n--XYZ\r\n"));
        assert!(text.contains("name=\"files[0]\"; filename=\"evil___name.txt\"\r\n"));
        assert!(text.ends_with("\r\n\r\n\u{0}data\r\n--XYZ--\r\n"));
    }
}
