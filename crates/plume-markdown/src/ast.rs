use plume_core::Snowflake;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Text(String),
    Bold(Vec<Node>),
    Italic(Vec<Node>),
    Underline(Vec<Node>),
    Strikethrough(Vec<Node>),
    Spoiler(Vec<Node>),
    InlineCode(String),
    CodeBlock {
        language: Option<String>,
        code: String,
    },
    /// One quoted line (`> `) or the rest of the message (`>>> `).
    Quote(Vec<Node>),
    Heading {
        level: u8,
        children: Vec<Node>,
    },
    /// Small grey text (`-# `).
    Subtext(Vec<Node>),
    UserMention(Snowflake),
    RoleMention(Snowflake),
    ChannelMention(Snowflake),
    Everyone,
    Here,
    CustomEmoji {
        name: String,
        id: Snowflake,
        animated: bool,
    },
    /// `<t:unix:style>`; `style` is Discord's letter (R = relative, f = full…).
    Timestamp {
        unix: i64,
        style: Option<char>,
    },
    Link(String),
    LineBreak,
}

/// Formatting in effect for a span. Styles nest, so several can be on.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub spoiler: bool,
    pub quote: bool,
    pub heading: bool,
    pub subtext: bool,
}

/// What a span contains. Borrowed from the tree, so flattening is cheap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Piece<'a> {
    Text(&'a str),
    Code(&'a str),
    CodeBlock {
        language: Option<&'a str>,
        code: &'a str,
    },
    UserMention(Snowflake),
    RoleMention(Snowflake),
    ChannelMention(Snowflake),
    Everyone,
    Here,
    CustomEmoji {
        name: &'a str,
        id: Snowflake,
        animated: bool,
    },
    Timestamp {
        unix: i64,
        style: Option<char>,
    },
    Link(&'a str),
    LineBreak,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span<'a> {
    pub piece: Piece<'a>,
    pub style: Style,
}

/// Walks the tree and returns its leaves in order, each with its style.
pub fn flatten(nodes: &[Node]) -> Vec<Span<'_>> {
    let mut spans = Vec::new();
    walk(nodes, Style::default(), &mut spans);
    spans
}

fn walk<'a>(nodes: &'a [Node], style: Style, out: &mut Vec<Span<'a>>) {
    for node in nodes {
        let mut push = |piece| out.push(Span { piece, style });
        match node {
            Node::Text(text) => push(Piece::Text(text)),
            Node::InlineCode(code) => push(Piece::Code(code)),
            Node::CodeBlock { language, code } => push(Piece::CodeBlock {
                language: language.as_deref(),
                code,
            }),
            Node::UserMention(id) => push(Piece::UserMention(*id)),
            Node::RoleMention(id) => push(Piece::RoleMention(*id)),
            Node::ChannelMention(id) => push(Piece::ChannelMention(*id)),
            Node::Everyone => push(Piece::Everyone),
            Node::Here => push(Piece::Here),
            Node::CustomEmoji { name, id, animated } => push(Piece::CustomEmoji {
                name,
                id: *id,
                animated: *animated,
            }),
            Node::Timestamp { unix, style } => push(Piece::Timestamp {
                unix: *unix,
                style: *style,
            }),
            Node::Link(url) => push(Piece::Link(url)),
            Node::LineBreak => push(Piece::LineBreak),
            Node::Bold(children) => walk(children, Style { bold: true, ..style }, out),
            Node::Italic(children) => walk(children, Style { italic: true, ..style }, out),
            Node::Underline(children) => walk(
                children,
                Style {
                    underline: true,
                    ..style
                },
                out,
            ),
            Node::Strikethrough(children) => walk(
                children,
                Style {
                    strikethrough: true,
                    ..style
                },
                out,
            ),
            Node::Spoiler(children) => walk(children, Style { spoiler: true, ..style }, out),
            Node::Quote(children) => walk(children, Style { quote: true, ..style }, out),
            Node::Heading { children, .. } => walk(
                children,
                Style {
                    heading: true,
                    bold: true,
                    ..style
                },
                out,
            ),
            Node::Subtext(children) => walk(children, Style { subtext: true, ..style }, out),
        }
    }
}
