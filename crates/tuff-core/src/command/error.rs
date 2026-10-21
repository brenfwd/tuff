//! Command failures as the game reports them: a translatable message, and
//! for parse errors the position in the input, shown as a second line
//! pointing at the mistake.

use tuff_item::text::TextComponent;

use super::reader::Reader;

/// Characters of input shown before the error position.
const CONTEXT_CHARS: usize = 10;

#[derive(Debug, Clone, PartialEq)]
pub struct CommandError {
    pub message: Box<TextComponent>,
    /// Where in the input the error is, for the context line.
    pub cursor: Option<usize>,
}

impl CommandError {
    pub fn simple(key: &str) -> Self {
        Self {
            message: Box::new(TextComponent::translate(key, [])),
            cursor: None,
        }
    }

    pub fn dynamic(key: &str, args: impl IntoIterator<Item = impl Into<TextComponent>>) -> Self {
        Self {
            message: Box::new(TextComponent::translate(
                key,
                args.into_iter().map(Into::into),
            )),
            cursor: None,
        }
    }

    pub fn text(message: TextComponent) -> Self {
        Self {
            message: Box::new(message),
            cursor: None,
        }
    }

    /// A failure with nothing to show, like `return fail`.
    pub fn silent() -> Self {
        Self::text(TextComponent::new(""))
    }

    pub fn is_silent(&self) -> bool {
        self.cursor.is_none() && *self.message == TextComponent::new("")
    }

    /// Attaches the reader's position, like the game's `createWithContext`.
    pub fn at(mut self, reader: &Reader<'_>) -> Self {
        self.cursor = Some(reader.cursor());
        self
    }

    pub fn at_cursor(mut self, cursor: usize) -> Self {
        self.cursor = Some(cursor);
        self
    }

    pub fn key(&self) -> Option<&str> {
        self.message.translate.as_deref()
    }

    /// The lines shown to whoever ran `input`: the message in red, and for
    /// a positioned error the surrounding input with the rest underlined.
    pub fn display(&self, input: &str) -> Vec<TextComponent> {
        if self.is_silent() {
            return Vec::new();
        }
        let mut lines = vec![(*self.message).clone().color("red")];
        if let Some(cursor) = self.cursor {
            let cursor = input
                .char_indices()
                .map(|(i, _)| i)
                .chain([input.len()])
                .rfind(|i| *i <= cursor)
                .unwrap_or(0)
                .min(input.len());
            let before = &input[..cursor];
            let shown_start = before
                .char_indices()
                .rev()
                .nth(CONTEXT_CHARS - 1)
                .map_or(0, |(i, _)| i);
            let mut context = TextComponent::new("").color("gray");
            if before.chars().count() > CONTEXT_CHARS {
                context = context.append("...");
            }
            context = context.append(&before[shown_start..]);
            if cursor < input.len() {
                context = context.append(
                    TextComponent::new(&input[cursor..])
                        .color("red")
                        .underlined(true),
                );
            }
            context = context.append(
                TextComponent::translate("command.context.here", [])
                    .color("red")
                    .italic(true),
            );
            lines.push(context);
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tuff_core::watchdog]
    #[test]
    fn display_shows_context_around_the_cursor() {
        let err = CommandError::simple("command.unknown.argument").at_cursor(18);
        let lines = err.display("gamemode creative extra");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].color.as_deref(), Some("red"));
        let context = &lines[1];
        assert_eq!(context.extra[0].text, "...");
        assert_eq!(context.extra[1].text, " creative ");
        assert_eq!(context.extra[2].text, "extra");
        assert_eq!(context.extra[2].underlined, Some(true));
        assert_eq!(
            context.extra[3].translate.as_deref(),
            Some("command.context.here")
        );
        let short = CommandError::simple("x").at_cursor(3).display("abc");
        assert_eq!(short[1].extra[0].text, "abc");
        assert_eq!(short[1].extra.len(), 2);
        assert_eq!(CommandError::simple("x").display("abc").len(), 1);
    }
}
