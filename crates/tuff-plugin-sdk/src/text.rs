// SPDX-License-Identifier: MIT
//! Colored chat text. Any string becomes a [`Span`] with a [`Styled`]
//! method (`"Hello".gold()`, `player.name.aqua().bold()`), and [`text!`]
//! joins spans and plain values into one message:
//!
//! ```
//! use tuff_plugin_sdk::text;
//! use tuff_plugin_sdk::text::Styled;
//!
//! let line = text!("Hello".gold(), ", ", "Alex".aqua().bold(), "!");
//! assert_eq!(line, "\u{a7}6Hello\u{a7}r, \u{a7}b\u{a7}lAlex\u{a7}r!");
//! ```
//!
//! A span renders as its color code, its style codes, the text and a
//! reset, using the legacy formatting codes the vanilla client renders
//! inside plain chat text; an unstyled span is just its text. The C++ SDK
//! renders spans the same way.
//!
//! [`text!`]: crate::text!

use std::fmt;

pub use crate::generated::{Color, Style, Styled};

/// The section sign that starts a formatting code.
pub const SECTION: char = '\u{a7}';

/// Text in one color and set of styles.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub color: Option<Color>,
    styles: u8,
}

impl Span {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            color: None,
            styles: 0,
        }
    }

    pub fn with_color(mut self, color: Color) -> Self {
        self.color = Some(color);
        self
    }

    pub fn with_style(mut self, style: Style) -> Self {
        self.styles |= 1 << style_bit(style);
        self
    }

    pub fn has(&self, style: Style) -> bool {
        self.styles & (1 << style_bit(style)) != 0
    }
}

fn style_bit(style: Style) -> usize {
    Style::ALL.iter().position(|s| *s == style).unwrap_or(0)
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.color.is_none() && self.styles == 0 {
            return f.write_str(&self.text);
        }
        if let Some(color) = self.color {
            write!(f, "{SECTION}{}", color.code())?;
        }
        for style in Style::ALL.into_iter().filter(|s| self.has(*s)) {
            write!(f, "{SECTION}{}", style.code())?;
        }
        write!(f, "{}{SECTION}r", self.text)
    }
}

impl From<&str> for Span {
    fn from(text: &str) -> Self {
        Span::new(text)
    }
}

impl From<String> for Span {
    fn from(text: String) -> Self {
        Span::new(text)
    }
}

impl From<&String> for Span {
    fn from(text: &String) -> Self {
        Span::new(text.as_str())
    }
}

impl Styled for str {
    fn color(&self, color: Color) -> Span {
        Span::new(self).with_color(color)
    }

    fn style(&self, style: Style) -> Span {
        Span::new(self).with_style(style)
    }
}

const COLOR_NAMES: [&str; 16] = [
    "black",
    "dark_blue",
    "dark_green",
    "dark_aqua",
    "dark_red",
    "dark_purple",
    "gold",
    "gray",
    "dark_gray",
    "blue",
    "green",
    "aqua",
    "red",
    "light_purple",
    "yellow",
    "white",
];

/// A message of styled parts that may carry click and hover events, sent
/// with [`PlayerExt::tell_rich`](crate::PlayerExt::tell_rich).
///
/// ```
/// use tuff_plugin_sdk::text::{Rich, Styled};
///
/// let message = Rich::new()
///     .plain("Click ")
///     .part("here".aqua().bold(), |part| part.run_command("/spawn").hover("Go to spawn"));
/// assert_eq!(message.spans().len(), 2);
/// ```
#[derive(Clone, Debug, Default)]
pub struct Rich {
    spans: Vec<crate::types::RichSpan>,
}

/// Click and hover events for one [`Rich`] part.
#[derive(Clone, Debug, Default)]
pub struct Events {
    click: Option<crate::types::RichClick>,
    hover: Option<String>,
    insertion: Option<String>,
}

impl Events {
    pub fn suggest_command(mut self, command: impl Into<String>) -> Self {
        self.click = Some(crate::types::RichClick::SuggestCommand(command.into()));
        self
    }

    /// Runs `command` (starting with `/`) as the player on click.
    pub fn run_command(mut self, command: impl Into<String>) -> Self {
        self.click = Some(crate::types::RichClick::RunCommand(command.into()));
        self
    }

    /// Opens an `http` or `https` URL after the client's confirmation.
    pub fn open_url(mut self, url: impl Into<String>) -> Self {
        self.click = Some(crate::types::RichClick::OpenUrl(url.into()));
        self
    }

    pub fn copy(mut self, text: impl Into<String>) -> Self {
        self.click = Some(crate::types::RichClick::Copy(text.into()));
        self
    }

    /// Plain tooltip text shown while the cursor is over the part.
    pub fn hover(mut self, text: impl Into<String>) -> Self {
        self.hover = Some(text.into());
        self
    }

    /// Text shift-click inserts into the chat box.
    pub fn insertion(mut self, text: impl Into<String>) -> Self {
        self.insertion = Some(text.into());
        self
    }
}

impl Rich {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds unstyled text.
    pub fn plain(self, text: impl Into<String>) -> Self {
        self.part(Span::new(text), |events| events)
    }

    /// Adds a styled part with the events `events` builds.
    pub fn part(mut self, span: impl Into<Span>, events: impl FnOnce(Events) -> Events) -> Self {
        let span = span.into();
        let events = events(Events::default());
        self.spans.push(crate::types::RichSpan {
            color: span
                .color
                .and_then(|color| Color::ALL.iter().position(|c| *c == color))
                .map(|index| COLOR_NAMES[index].to_owned()),
            bold: span.has(Style::Bold),
            italic: span.has(Style::Italic),
            underlined: span.has(Style::Underlined),
            text: span.text,
            click: events.click,
            hover: events.hover,
            insertion: events.insertion,
        });
        self
    }

    pub fn spans(&self) -> &[crate::types::RichSpan] {
        &self.spans
    }
}

/// `text` without its formatting codes, for logs.
pub fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == SECTION {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

/// Joins [`Span`]s and other displayable values into one `String`.
#[macro_export]
macro_rules! text {
    ($($part:expr),* $(,)?) => {{
        let mut out = ::std::string::String::new();
        $(::std::fmt::Write::write_fmt(&mut out, ::core::format_args!("{}", $part))
            .expect("writing to a String");)*
        out
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn spans_render_color_then_styles_then_reset() {
        assert_eq!("plain".to_string(), Span::from("plain").to_string());
        assert_eq!("a".red().to_string(), "\u{a7}ca\u{a7}r");
        assert_eq!(
            "b".italic().bold().dark_gray().to_string(),
            "\u{a7}8\u{a7}l\u{a7}ob\u{a7}r"
        );
        let n = 3;
        assert_eq!(crate::text!("x".gold(), " ", n, "!"), "\u{a7}6x\u{a7}r 3!");
        assert_eq!(plain(&crate::text!("Hi".aqua().underlined(), "!")), "Hi!");
    }
}
