//! Turns a channel's messages into wrapped, styled terminal lines.

use chrono::{DateTime, Local, TimeZone};
use plume_core::models::Message;
use plume_core::{Snowflake, Store};
use plume_markdown::{Piece, Style as MdStyle};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// A run of text with one style. A "logical line" is a Vec of these, before wrapping.
type Segment = (String, Style);

const INDENT: usize = 2;
/// Messages from the same person this close together share one header.
const GROUP_WINDOW_MS: u64 = 7 * 60 * 1000;

const DIM: Style = Style::new().fg(Color::DarkGray);
const CODE: Style = Style::new().fg(Color::Yellow);
const MENTION: Style = Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD);
const MENTION_ME: Style = Style::new()
    .fg(Color::Black)
    .bg(Color::Yellow)
    .add_modifier(Modifier::BOLD);
const LINK: Style = Style::new().fg(Color::Blue).add_modifier(Modifier::UNDERLINED);
const EMOJI: Style = Style::new().fg(Color::Magenta);

/// Colours for usernames, picked by user ID so each person keeps theirs.
const NAME_COLORS: [Color; 6] = [
    Color::LightRed,
    Color::LightGreen,
    Color::LightYellow,
    Color::LightBlue,
    Color::LightMagenta,
    Color::LightCyan,
];

/// Every line for a channel's cached messages, oldest first.
pub fn channel_lines(store: &Store, channel_id: Snowflake, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width).max(INDENT + 10);
    let mut lines = Vec::new();
    let mut previous: Option<&Message> = None;

    for message in store.messages(channel_id) {
        let time = local_time(message.timestamp_ms());
        let new_day = previous.is_none_or(|p| local_time(p.timestamp_ms()).date_naive() != time.date_naive());
        if new_day {
            lines.push(day_separator(&time, width));
        }

        if let Some(reply_to) = message.reply_to {
            lines.push(reply_line(store, message.channel_id, reply_to, width));
        }

        let grouped = !new_day
            && message.reply_to.is_none()
            && previous.is_some_and(|p| {
                p.author_id == message.author_id && message.timestamp_ms() - p.timestamp_ms() < GROUP_WINDOW_MS
            });
        if !grouped {
            lines.push(header_line(store, message, &time));
        }

        for logical in content_lines(store, message) {
            lines.extend(wrap(&logical, width, INDENT));
        }
        previous = Some(message);
    }
    lines
}

fn header_line(store: &Store, message: &Message, time: &DateTime<Local>) -> Line<'static> {
    let name = store
        .user(message.author_id)
        .map_or_else(|| "unknown".to_owned(), |u| u.display_name().to_owned());
    Line::from(vec![
        Span::styled(name, name_style(message.author_id)),
        Span::styled(format!("  {}", time.format("%H:%M")), DIM),
    ])
}

fn reply_line(store: &Store, channel_id: Snowflake, reply_to: Snowflake, width: usize) -> Line<'static> {
    let text = match store.message(channel_id, reply_to) {
        Some(original) => {
            let name = store.user(original.author_id).map_or("unknown", |u| u.display_name());
            let snippet: String = original.content.lines().next().unwrap_or("").to_owned();
            format!("╭ {name}: {snippet}")
        }
        None => "╭ (reply to an older message)".to_owned(),
    };
    Line::from(Span::styled(truncate(&text, width), DIM))
}

fn day_separator(time: &DateTime<Local>, width: usize) -> Line<'static> {
    let label = format!(" {} ", time.format("%a %-d %b %Y"));
    let side = width.saturating_sub(label.width()) / 2;
    Line::from(Span::styled(
        format!("{}{label}{}", "─".repeat(side), "─".repeat(side)),
        DIM,
    ))
}

/// The message body as logical lines: markdown, then attachments, then "(edited)".
fn content_lines(store: &Store, message: &Message) -> Vec<Vec<Segment>> {
    let me = store.me().map(|u| u.id);
    let nodes = plume_markdown::parse(&message.content);
    let spans = plume_markdown::flatten(&nodes);

    let mut lines: Vec<Vec<Segment>> = Vec::new();
    let mut current: Vec<Segment> = Vec::new();

    for span in spans {
        if span.style.quote && current.is_empty() {
            current.push(("▎ ".to_owned(), DIM));
        }
        let style = to_tui_style(span.style);
        match span.piece {
            Piece::Text(text) => current.push((text.to_owned(), style)),
            Piece::Code(code) => current.push((code.to_owned(), CODE)),
            Piece::CodeBlock { language, code } => {
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }
                if let Some(language) = language {
                    lines.push(vec![(format!("┌ {language}"), DIM)]);
                }
                for code_line in code.lines() {
                    lines.push(vec![("│ ".to_owned(), DIM), (code_line.to_owned(), CODE)]);
                }
            }
            Piece::LineBreak => lines.push(std::mem::take(&mut current)),
            Piece::UserMention(id) => {
                let name = store.user(id).map_or("unknown-user", |u| u.display_name());
                let style = if Some(id) == me { MENTION_ME } else { MENTION };
                current.push((format!("@{name}"), style));
            }
            Piece::ChannelMention(id) => {
                let name = store.channel(id).and_then(|c| c.name.as_deref()).unwrap_or("unknown");
                current.push((format!("#{name}"), MENTION));
            }
            Piece::RoleMention(_) => current.push(("@role".to_owned(), MENTION)),
            Piece::Everyone => current.push(("@everyone".to_owned(), MENTION_ME)),
            Piece::Here => current.push(("@here".to_owned(), MENTION_ME)),
            Piece::CustomEmoji { name, .. } => current.push((format!(":{name}:"), EMOJI)),
            Piece::Timestamp { unix, style } => current.push((format_timestamp(unix, style), MENTION)),
            Piece::Link(url) => current.push((url.to_owned(), LINK)),
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }

    for attachment in &message.attachments {
        lines.push(vec![
            (
                format!("📎 {} ({}) ", attachment.filename, human_size(attachment.size)),
                DIM,
            ),
            (attachment.url.clone(), LINK),
        ]);
    }
    if lines.is_empty() {
        lines.push(vec![("(no text)".to_owned(), DIM)]);
    }
    if message.edited
        && let Some(last) = lines.last_mut()
    {
        last.push((" (edited)".to_owned(), DIM));
    }
    lines
}

fn to_tui_style(style: MdStyle) -> Style {
    let mut out = Style::new();
    if style.bold {
        out = out.add_modifier(Modifier::BOLD);
    }
    if style.italic {
        out = out.add_modifier(Modifier::ITALIC);
    }
    if style.underline {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    if style.strikethrough {
        out = out.add_modifier(Modifier::CROSSED_OUT);
    }
    if style.subtext {
        out = out.fg(Color::DarkGray);
    }
    if style.spoiler {
        // Same colour on same colour: hidden until you select the text.
        out = out.fg(Color::DarkGray).bg(Color::DarkGray);
    }
    out
}

/// Greedy word wrap. Words longer than a whole line are split by character.
pub fn wrap(segments: &[Segment], width: usize, indent: usize) -> Vec<Line<'static>> {
    let available = width.saturating_sub(indent).max(1);
    let mut lines = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut used = 0;

    let mut finish = |current: &mut Vec<Span<'static>>, used: &mut usize| {
        let mut spans = vec![Span::raw(" ".repeat(indent))];
        spans.append(current);
        lines.push(Line::from(spans));
        *used = 0;
    };

    for (text, style) in segments {
        for word in text.split_inclusive(' ') {
            let mut word = word;
            if used + word.width() > available && used > 0 {
                finish(&mut current, &mut used);
                word = word.trim_start();
            }
            if word.width() <= available - used {
                used += word.width();
                current.push(Span::styled(word.to_owned(), *style));
                continue;
            }
            // Too long for any line: hard-split it.
            let mut chunk = String::new();
            for c in word.chars() {
                let w = c.width().unwrap_or(0);
                if used + w > available {
                    current.push(Span::styled(std::mem::take(&mut chunk), *style));
                    finish(&mut current, &mut used);
                }
                chunk.push(c);
                used += w;
            }
            if !chunk.is_empty() {
                current.push(Span::styled(chunk, *style));
            }
        }
    }
    finish(&mut current, &mut used);
    lines
}

fn name_style(user_id: Snowflake) -> Style {
    let color = NAME_COLORS[(user_id.0 % NAME_COLORS.len() as u64) as usize];
    Style::new().fg(color).add_modifier(Modifier::BOLD)
}

fn local_time(unix_ms: u64) -> DateTime<Local> {
    Local
        .timestamp_millis_opt(unix_ms as i64)
        .single()
        .unwrap_or_else(Local::now)
}

/// Discord's `<t:…:style>` formats.
fn format_timestamp(unix: i64, style: Option<char>) -> String {
    let Some(time) = Local.timestamp_opt(unix, 0).single() else {
        return "(invalid date)".to_owned();
    };
    match style {
        Some('t') => time.format("%H:%M").to_string(),
        Some('T') => time.format("%H:%M:%S").to_string(),
        Some('d') => time.format("%d/%m/%Y").to_string(),
        Some('D') => time.format("%-d %B %Y").to_string(),
        Some('F') => time.format("%A, %-d %B %Y %H:%M").to_string(),
        Some('R') => relative(unix - Local::now().timestamp()),
        _ => time.format("%-d %B %Y %H:%M").to_string(),
    }
}

fn relative(seconds: i64) -> String {
    let amount = seconds.unsigned_abs();
    let (n, unit) = match amount {
        0..60 => (amount, "second"),
        60..3600 => (amount / 60, "minute"),
        3600..86_400 => (amount / 3600, "hour"),
        86_400..2_592_000 => (amount / 86_400, "day"),
        2_592_000..31_536_000 => (amount / 2_592_000, "month"),
        _ => (amount / 31_536_000, "year"),
    };
    let plural = if n == 1 { "" } else { "s" };
    if seconds >= 0 {
        format!("in {n} {unit}{plural}")
    } else {
        format!("{n} {unit}{plural} ago")
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Cuts a string to `width` columns, adding "…" if it was longer.
pub fn truncate(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn wraps_at_word_boundaries_with_indent() {
        let lines = wrap(&[("hello big wide world".to_owned(), Style::new())], 12, 2);
        assert_eq!(plain(&lines), ["  hello big ", "  wide world"]);
    }

    #[test]
    fn splits_words_longer_than_a_line() {
        let lines = wrap(&[("abcdefghij".to_owned(), Style::new())], 6, 2);
        assert_eq!(plain(&lines), ["  abcd", "  efgh", "  ij"]);
    }

    #[test]
    fn wide_characters_count_double() {
        let lines = wrap(&[("🎉🎉🎉".to_owned(), Style::new())], 6, 2);
        assert_eq!(plain(&lines), ["  🎉🎉", "  🎉"]);
    }

    #[test]
    fn empty_line_still_takes_a_row() {
        assert_eq!(wrap(&[], 10, 2).len(), 1);
    }

    #[test]
    fn sizes_and_relative_times() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(relative(-120), "2 minutes ago");
        assert_eq!(relative(3600), "in 1 hour");
    }

    #[test]
    fn truncates_with_ellipsis() {
        assert_eq!(truncate("hello world", 6), "hello…");
        assert_eq!(truncate("hi", 6), "hi");
    }
}
