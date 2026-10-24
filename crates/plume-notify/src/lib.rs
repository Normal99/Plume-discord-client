//! Desktop notifications: the freedesktop notification service over D-Bus
//! on Linux and the BSDs, Notification Center on macOS. Elsewhere (Windows),
//! [`send`] returns [`Unsupported`] and the caller can fall back to a
//! terminal notification.
//!
//! [`send`] blocks for a few milliseconds; call it from a blocking task.

mod desktop;

pub use desktop::{Unsupported, send};

/// Shortens text for a notification body, on a character boundary.
pub fn shorten(text: &str, max_chars: usize) -> String {
    let mut out: String = text.chars().take(max_chars).collect();
    if text.chars().nth(max_chars).is_some() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn shorten_respects_characters() {
        assert_eq!(super::shorten("héllo wörld", 5), "héllo…");
        assert_eq!(super::shorten("hi", 5), "hi");
    }
}
