//! Discord-flavoured markdown → a small syntax tree.
//!
//! [`parse`] builds the tree; [`flatten`] turns it into styled spans, which
//! is what most renderers actually want. Anything that doesn't parse as
//! markdown stays plain text, so no input can make this fail.
//!
//! Supported: `**bold**`, `*italic*`/`_italic_`, `__underline__`,
//! `~~strike~~`, `||spoiler||`, `` `code` ``, ```` ```code blocks``` ````,
//! `> quotes`, `>>> quotes`, `# headings`, `-# subtext`, `\` escapes,
//! links, `<@user>` `<@&role>` `<#channel>` mentions, `@everyone`/`@here`,
//! custom emoji, and `<t:…>` timestamps.

mod ast;
mod emoji;
mod mentions;
mod parser;
mod shortcodes;

pub use ast::{Node, Piece, Span, Style, flatten};
pub use parser::parse;
pub use shortcodes::{replace_shortcodes, shortcode};
