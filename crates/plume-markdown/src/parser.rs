//! A hand-written parser in three layers:
//!
//! 1. code blocks (```` ``` ````), whose contents are never parsed,
//! 2. lines: quotes, headings and subtext,
//! 3. inline formatting, mentions and links, recursively.
//!
//! The rules follow Discord's own parser (simple-markdown) closely enough
//! for real-world messages. When a delimiter has no partner, it's just text.

use crate::ast::Node;
use crate::mentions::parse_angle;

pub fn parse(input: &str) -> Vec<Node> {
    let mut out = Vec::new();
    let mut rest = input;
    while let Some((start, block, end)) = find_code_block(rest) {
        lines(rest[..start].strip_suffix('\n').unwrap_or(&rest[..start]), &mut out);
        out.push(block);
        rest = &rest[end..];
        rest = rest.strip_prefix('\n').unwrap_or(rest);
    }
    lines(rest, &mut out);
    out
}

/// Finds the first ```` ```…``` ```` block: (start byte, node, end byte).
fn find_code_block(s: &str) -> Option<(usize, Node, usize)> {
    let start = s.find("```")?;
    let body_start = start + 3;
    let close = body_start + s[body_start..].find("```")?;
    let body = &s[body_start..close];
    if body.trim().is_empty() {
        return None;
    }

    // ```rust\n…``` — a lone word on the first line names the language.
    let (language, code) = match body.split_once('\n') {
        Some((first, code))
            if !first.is_empty() && first.chars().all(|c| c.is_alphanumeric() || "+-#._".contains(c)) =>
        {
            (Some(first.to_owned()), code)
        }
        _ => (None, body),
    };
    let code = code.strip_suffix('\n').unwrap_or(code);
    Some((
        start,
        Node::CodeBlock {
            language,
            code: code.to_owned(),
        },
        close + 3,
    ))
}

/// Line-level syntax, with a `LineBreak` between lines.
fn lines(text: &str, out: &mut Vec<Node>) {
    if text.is_empty() {
        return;
    }
    let mut offset = 0;
    loop {
        let line_end = text[offset..].find('\n').map_or(text.len(), |i| offset + i);
        let line = &text[offset..line_end];
        if offset > 0 {
            out.push(Node::LineBreak);
        }

        if line.starts_with(">>> ") {
            // Everything from here to the end is one quote.
            out.push(Node::Quote(inline_lines(&text[offset + 4..])));
            return;
        }
        if let Some(quoted) = line.strip_prefix("> ") {
            out.push(Node::Quote(inline(quoted)));
        } else if let Some(small) = line.strip_prefix("-# ") {
            out.push(Node::Subtext(inline(small)));
        } else if let Some((level, title)) = heading(line) {
            out.push(Node::Heading {
                level,
                children: inline(title),
            });
        } else {
            out.extend(inline(line));
        }

        if line_end == text.len() {
            return;
        }
        offset = line_end + 1;
    }
}

fn heading(line: &str) -> Option<(u8, &str)> {
    for (level, prefix) in [(3, "### "), (2, "## "), (1, "# ")] {
        if let Some(title) = line.strip_prefix(prefix) {
            return Some((level, title));
        }
    }
    None
}

/// Inline syntax over several lines (inside a `>>>` quote).
fn inline_lines(text: &str) -> Vec<Node> {
    let mut out = Vec::new();
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push(Node::LineBreak);
        }
        out.extend(inline(line));
    }
    out
}

pub(crate) fn inline(s: &str) -> Vec<Node> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut prev: Option<char> = None;
    let mut i = 0;

    while i < s.len() {
        let rest = &s[i..];
        if let Some((node, used)) = token(rest, prev) {
            if !text.is_empty() {
                out.push(Node::Text(std::mem::take(&mut text)));
            }
            out.push(node);
            prev = s[..i + used].chars().next_back();
            i += used;
            continue;
        }

        let Some(c) = rest.chars().next() else { break };
        // `\*` means a literal `*`.
        if c == '\\'
            && let Some(next) = rest[1..].chars().next()
            && next.is_ascii_punctuation()
        {
            text.push(next);
            prev = Some(next);
            i += 1 + next.len_utf8();
            continue;
        }
        text.push(c);
        prev = Some(c);
        i += c.len_utf8();
    }

    if !text.is_empty() {
        out.push(Node::Text(text));
    }
    out
}

/// Tries every inline rule at the start of `s`.
fn token(s: &str, prev: Option<char>) -> Option<(Node, usize)> {
    match s.as_bytes().first()? {
        b'`' => inline_code(s),
        b'|' => wrapped(s, "||", None).map(|(inner, used)| (Node::Spoiler(inline(inner)), used)),
        b'~' => wrapped(s, "~~", None).map(|(inner, used)| (Node::Strikethrough(inline(inner)), used)),
        b'*' => {
            if let Some((inner, used)) = wrapped(s, "***", Some('*')) {
                return Some((Node::Bold(vec![Node::Italic(inline(inner))]), used));
            }
            if let Some((inner, used)) = wrapped(s, "**", Some('*')) {
                return Some((Node::Bold(inline(inner)), used));
            }
            star_italic(s)
        }
        b'_' => {
            if let Some((inner, used)) = wrapped(s, "__", Some('_')) {
                return Some((Node::Underline(inline(inner)), used));
            }
            underscore_italic(s, prev)
        }
        b'<' => parse_angle(s),
        b'@' => {
            if s.starts_with("@everyone") {
                Some((Node::Everyone, "@everyone".len()))
            } else if s.starts_with("@here") {
                Some((Node::Here, "@here".len()))
            } else {
                None
            }
        }
        b'h' if !prev.is_some_and(char::is_alphanumeric) => link(s),
        _ => None,
    }
}

/// `delim inner delim`, with a non-empty `inner`. The closing delimiter
/// must not be followed by `not_followed_by` (so `**a***` closes at the end).
/// Returns the inner text and the total bytes used.
fn wrapped<'a>(s: &'a str, delim: &str, not_followed_by: Option<char>) -> Option<(&'a str, usize)> {
    let body = s.strip_prefix(delim)?;
    let mut i = 0;
    while i < body.len() {
        let rest = &body[i..];
        if let Some(escaped) = rest.strip_prefix('\\') {
            i += 1 + escaped.chars().next().map_or(0, char::len_utf8);
            continue;
        }
        if i > 0 && rest.starts_with(delim) {
            let after = &rest[delim.len()..];
            if not_followed_by.is_none_or(|c| !after.starts_with(c)) {
                return Some((&body[..i], i + 2 * delim.len()));
            }
        }
        i += rest.chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// `*italic*`. Can contain `**bold**`; can't start or end with a space.
fn star_italic(s: &str) -> Option<(Node, usize)> {
    let body = s.strip_prefix('*')?;
    if body.starts_with(char::is_whitespace) {
        return None;
    }
    let mut i = 0;
    let mut prev = None;
    while i < body.len() {
        let rest = &body[i..];
        if let Some(escaped) = rest.strip_prefix('\\') {
            i += 1 + escaped.chars().next().map_or(0, char::len_utf8);
            prev = Some('\\');
            continue;
        }
        if rest.starts_with("**") {
            i += 2;
            prev = Some('*');
            continue;
        }
        if rest.starts_with('*') && i > 0 && !prev.is_some_and(char::is_whitespace) {
            return Some((Node::Italic(inline(&body[..i])), i + 2));
        }
        let c = rest.chars().next()?;
        prev = Some(c);
        i += c.len_utf8();
    }
    None
}

/// `_italic_`, but only at word edges, so `snake_case_names` stay as they are.
fn underscore_italic(s: &str, prev: Option<char>) -> Option<(Node, usize)> {
    if prev.is_some_and(char::is_alphanumeric) {
        return None;
    }
    let body = s.strip_prefix('_')?;
    let mut i = 0;
    while i < body.len() {
        let rest = &body[i..];
        if let Some(escaped) = rest.strip_prefix('\\') {
            i += 1 + escaped.chars().next().map_or(0, char::len_utf8);
            continue;
        }
        if rest.starts_with("__") {
            i += 2;
            continue;
        }
        if rest.starts_with('_') && i > 0 {
            let next = rest[1..].chars().next();
            if !next.is_some_and(char::is_alphanumeric) {
                return Some((Node::Italic(inline(&body[..i])), i + 2));
            }
        }
        i += rest.chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// `` `code` `` or ``` ``co`de`` ```: the closing run must be exactly as long.
fn inline_code(s: &str) -> Option<(Node, usize)> {
    let ticks = s.bytes().take_while(|&b| b == b'`').count();
    let body = &s[ticks..];
    let mut i = 0;
    while i < body.len() {
        let rest = &body[i..];
        if rest.starts_with('`') {
            let run = rest.bytes().take_while(|&b| b == b'`').count();
            if run == ticks && i > 0 {
                return Some((Node::InlineCode(body[..i].to_owned()), 2 * ticks + i));
            }
            i += run;
            continue;
        }
        i += rest.chars().next().map_or(1, char::len_utf8);
    }
    None
}

/// A bare `http(s)://` link, minus trailing punctuation like a final `.`.
fn link(s: &str) -> Option<(Node, usize)> {
    let scheme = if s.starts_with("https://") {
        "https://".len()
    } else if s.starts_with("http://") {
        "http://".len()
    } else {
        return None;
    };
    let end = s.find(|c: char| c.is_whitespace() || c == '<').unwrap_or(s.len());
    let mut url = &s[..end];
    // Keep a final `)` when it closes a `(` in the URL (Wikipedia links).
    while let Some(last) = url.chars().next_back() {
        let balanced_paren = last == ')' && url.matches('(').count() >= url.matches(')').count();
        if !".,:;!?)\"'".contains(last) || balanced_paren {
            break;
        }
        url = &url[..url.len() - 1];
    }
    if url.len() <= scheme {
        return None;
    }
    Some((Node::Link(url.to_owned()), url.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use plume_core::Snowflake;

    fn text(s: &str) -> Node {
        Node::Text(s.to_owned())
    }

    #[test]
    fn plain_text_stays_plain() {
        assert_eq!(parse("hello world"), [text("hello world")]);
        assert_eq!(parse(""), []);
    }

    #[test]
    fn basic_styles() {
        assert_eq!(parse("**b**"), [Node::Bold(vec![text("b")])]);
        assert_eq!(parse("*i*"), [Node::Italic(vec![text("i")])]);
        assert_eq!(parse("_i_"), [Node::Italic(vec![text("i")])]);
        assert_eq!(parse("__u__"), [Node::Underline(vec![text("u")])]);
        assert_eq!(parse("~~s~~"), [Node::Strikethrough(vec![text("s")])]);
        assert_eq!(parse("||secret||"), [Node::Spoiler(vec![text("secret")])]);
        assert_eq!(parse("***bi***"), [Node::Bold(vec![Node::Italic(vec![text("bi")])])]);
    }

    #[test]
    fn styles_nest() {
        assert_eq!(
            parse("*a **b** c*"),
            [Node::Italic(vec![text("a "), Node::Bold(vec![text("b")]), text(" c")])]
        );
        assert_eq!(parse("||**x**||"), [Node::Spoiler(vec![Node::Bold(vec![text("x")])])]);
    }

    #[test]
    fn unmatched_delimiters_are_text() {
        assert_eq!(parse("2 * 3 = 6"), [text("2 * 3 = 6")]);
        assert_eq!(parse("**open"), [text("**open")]);
        assert_eq!(parse("a || b"), [text("a || b")]);
        assert_eq!(parse("* not italic *"), [text("* not italic *")]);
    }

    #[test]
    fn snake_case_is_not_italic() {
        assert_eq!(parse("my_var_name"), [text("my_var_name")]);
        assert_eq!(
            parse("use _this_ one"),
            [text("use "), Node::Italic(vec![text("this")]), text(" one")]
        );
    }

    #[test]
    fn escapes_make_delimiters_literal() {
        assert_eq!(parse(r"\*not italic\*"), [text("*not italic*")]);
        assert_eq!(parse(r"a\b"), [text(r"a\b")], "only punctuation can be escaped");
    }

    #[test]
    fn inline_code_is_not_parsed_inside() {
        assert_eq!(parse("`**x**`"), [Node::InlineCode("**x**".into())]);
        assert_eq!(parse("``a`b``"), [Node::InlineCode("a`b".into())]);
        assert_eq!(
            parse("say `hi` now"),
            [text("say "), Node::InlineCode("hi".into()), text(" now")]
        );
    }

    #[test]
    fn code_blocks_with_and_without_language() {
        assert_eq!(
            parse("```rust\nfn main() {}\n```"),
            [Node::CodeBlock {
                language: Some("rust".into()),
                code: "fn main() {}".into()
            }]
        );
        assert_eq!(
            parse("before\n```\n**not bold**\n```\nafter"),
            [
                text("before"),
                Node::CodeBlock {
                    language: None,
                    code: "\n**not bold**".into()
                },
                text("after"),
            ]
        );
        assert_eq!(
            parse("```one line```"),
            [Node::CodeBlock {
                language: None,
                code: "one line".into()
            }]
        );
    }

    #[test]
    fn quotes_headings_and_subtext() {
        assert_eq!(
            parse("> quoted\nnormal"),
            [Node::Quote(vec![text("quoted")]), Node::LineBreak, text("normal")]
        );
        assert_eq!(
            parse("hi\n>>> all\nof this"),
            [
                text("hi"),
                Node::LineBreak,
                Node::Quote(vec![text("all"), Node::LineBreak, text("of this")])
            ]
        );
        assert_eq!(
            parse("## Title"),
            [Node::Heading {
                level: 2,
                children: vec![text("Title")]
            }]
        );
        assert_eq!(parse("-# small"), [Node::Subtext(vec![text("small")])]);
        assert_eq!(parse("#hashtag"), [text("#hashtag")]);
    }

    #[test]
    fn mentions_emoji_and_timestamps() {
        assert_eq!(
            parse("hi <@123> and <@!45> in <#6> with <@&7>"),
            [
                text("hi "),
                Node::UserMention(Snowflake(123)),
                text(" and "),
                Node::UserMention(Snowflake(45)),
                text(" in "),
                Node::ChannelMention(Snowflake(6)),
                text(" with "),
                Node::RoleMention(Snowflake(7)),
            ]
        );
        assert_eq!(
            parse("<a:party:99>"),
            [Node::CustomEmoji {
                name: "party".into(),
                id: Snowflake(99),
                animated: true
            }]
        );
        assert_eq!(
            parse("<t:1700000000:R>"),
            [Node::Timestamp {
                unix: 1_700_000_000,
                style: Some('R')
            }]
        );
        assert_eq!(parse("@everyone look"), [Node::Everyone, text(" look")]);
        assert_eq!(parse("a < b > c"), [text("a < b > c")]);
        assert_eq!(parse("<@notanid>"), [text("<@notanid>")]);
    }

    #[test]
    fn links_drop_trailing_punctuation() {
        assert_eq!(
            parse("see https://example.com/a."),
            [text("see "), Node::Link("https://example.com/a".into()), text(".")]
        );
        assert_eq!(
            parse("https://en.wikipedia.org/wiki/Rust_(programming_language)"),
            [Node::Link(
                "https://en.wikipedia.org/wiki/Rust_(programming_language)".into()
            )]
        );
        assert_eq!(
            parse("<https://quiet.example>"),
            [Node::Link("https://quiet.example".into())]
        );
        assert_eq!(parse("nothttps://x.y"), [text("nothttps://x.y")]);
    }

    #[test]
    fn non_ascii_text_is_safe() {
        assert_eq!(
            parse("héllo **wörld** 🎉"),
            [text("héllo "), Node::Bold(vec![text("wörld")]), text(" 🎉")]
        );
        // Odd input must never panic.
        for input in [
            "*", "**", "`", "```", "<", "<@", "||", "\\", "_", "~~~", "*🎉*", "_é_", "> ", ">>> ",
        ] {
            let _ = parse(input);
        }
    }

    #[test]
    fn never_panics_on_any_short_input() {
        // Every string up to 4 characters from the trickiest alphabet.
        let alphabet = [
            '*', '_', '`', '|', '~', '<', '>', '@', '\\', 'a', 'é', '\n', '#', ' ', '-', 'h',
        ];
        let mut inputs = vec![String::new()];
        for _ in 0..4 {
            let longer: Vec<String> = inputs
                .iter()
                .flat_map(|s| alphabet.iter().map(move |c| format!("{s}{c}")))
                .collect();
            for input in &longer {
                let nodes = parse(input);
                let _ = crate::flatten(&nodes);
            }
            inputs = longer;
        }
    }

    #[test]
    fn flatten_carries_styles() {
        let nodes = parse("**a *b***");
        let spans = crate::flatten(&nodes);
        assert_eq!(spans.len(), 2);
        assert!(spans[0].style.bold && !spans[0].style.italic);
        assert!(spans[1].style.bold && spans[1].style.italic);
    }
}
