//! The most used `:shortcode:` emoji, so they can be typed in the terminal
//! (Discord's own client converts them before sending; the API doesn't).
//! A full table is ~1,900 entries; these cover everyday use without the size.

const SHORTCODES: &[(&str, &str)] = &[
    ("+1", "👍"),
    ("-1", "👎"),
    ("100", "💯"),
    ("angry", "😠"),
    ("blush", "😊"),
    ("broken_heart", "💔"),
    ("clap", "👏"),
    ("cry", "😢"),
    ("eyes", "👀"),
    ("facepalm", "🤦"),
    ("fire", "🔥"),
    ("flushed", "😳"),
    ("grin", "😁"),
    ("grinning", "😀"),
    ("heart", "❤️"),
    ("heart_eyes", "😍"),
    ("heavy_check_mark", "✔️"),
    ("hugging", "🤗"),
    ("innocent", "😇"),
    ("joy", "😂"),
    ("kissing_heart", "😘"),
    ("laughing", "😆"),
    ("moyai", "🗿"),
    ("muscle", "💪"),
    ("neutral_face", "😐"),
    ("ok_hand", "👌"),
    ("partying_face", "🥳"),
    ("pensive", "😔"),
    ("pleading_face", "🥺"),
    ("point_up", "☝️"),
    ("pray", "🙏"),
    ("raised_hands", "🙌"),
    ("relieved", "😌"),
    ("rocket", "🚀"),
    ("rofl", "🤣"),
    ("rolling_eyes", "🙄"),
    ("scream", "😱"),
    ("shrug", "🤷"),
    ("skull", "💀"),
    ("slight_smile", "🙂"),
    ("smile", "😄"),
    ("smiley", "😃"),
    ("smirk", "😏"),
    ("sob", "😭"),
    ("sparkles", "✨"),
    ("star", "⭐"),
    ("sunglasses", "😎"),
    ("sweat_smile", "😅"),
    ("tada", "🎉"),
    ("thinking", "🤔"),
    ("thumbsdown", "👎"),
    ("thumbsup", "👍"),
    ("tired_face", "😫"),
    ("triumph", "😤"),
    ("unamused", "😒"),
    ("upside_down", "🙃"),
    ("wave", "👋"),
    ("weary", "😩"),
    ("white_check_mark", "✅"),
    ("wink", "😉"),
    ("x", "❌"),
    ("yum", "😋"),
    ("zany_face", "🤪"),
];

/// The emoji for a shortcode name (without the colons).
pub fn shortcode(name: &str) -> Option<&'static str> {
    SHORTCODES
        .binary_search_by(|(code, _)| (*code).cmp(name))
        .ok()
        .map(|index| SHORTCODES[index].1)
}

/// Replaces known `:shortcode:`s with their emoji, leaving code spans and
/// code blocks alone. Unknown names (custom emoji, typos) stay as typed.
pub fn replace_shortcodes(text: &str) -> String {
    if !text.contains(':') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    // Split on backticks: odd-numbered pieces are inside code.
    for (index, piece) in text.split('`').enumerate() {
        if index > 0 {
            out.push('`');
        }
        if index % 2 == 1 {
            out.push_str(piece);
        } else {
            replace_in_plain(piece, &mut out);
        }
    }
    out
}

fn replace_in_plain(text: &str, out: &mut String) {
    let mut rest = text;
    while let Some(start) = rest.find(':') {
        let after = &rest[start + 1..];
        let name_end = after.find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-')));
        match name_end {
            Some(end) if end > 0 && after[end..].starts_with(':') => {
                if let Some(emoji) = shortcode(&after[..end]) {
                    out.push_str(&rest[..start]);
                    out.push_str(emoji);
                    rest = &after[end + 1..];
                    continue;
                }
            }
            _ => {}
        }
        out.push_str(&rest[..=start]);
        rest = after;
    }
    out.push_str(rest);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_sorted_for_binary_search() {
        assert!(SHORTCODES.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(shortcode("fire"), Some("🔥"));
        assert_eq!(shortcode("nope"), None);
    }

    #[test]
    fn replaces_outside_code_only() {
        assert_eq!(replace_shortcodes("nice :fire: :+1:"), "nice 🔥 👍");
        assert_eq!(replace_shortcodes("`:fire:` :fire:"), "`:fire:` 🔥");
        assert_eq!(replace_shortcodes("```\n:fire:\n```"), "```\n:fire:\n```");
        assert_eq!(
            replace_shortcodes("time 12:30:45 :custom_thing: a:b"),
            "time 12:30:45 :custom_thing: a:b"
        );
        assert_eq!(replace_shortcodes("::fire::"), ":🔥:");
        assert_eq!(replace_shortcodes("héllo :tada:!"), "héllo 🎉!");
    }
}
