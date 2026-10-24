use std::fmt;
use std::sync::Arc;

/// A Discord token. It's full access to the account, so it never shows up
/// in `Debug` output or logs: you have to call [`Token::expose`] on purpose.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(Arc<str>);

impl Token {
    pub fn new(token: impl Into<String>) -> Self {
        Token(token.into().trim().into())
    }

    /// The raw token, for the `Authorization` header and IDENTIFY only.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_is_redacted() {
        let token = Token::new("  secret.token.value\n");
        assert_eq!(
            token.expose(),
            "secret.token.value",
            "whitespace from pasting is trimmed"
        );
        assert!(!format!("{token:?}").contains("secret"));
    }
}
