//! The interactive terminal console: a line editor with history and tab
//! completion when stdin and stdout are terminals, plain line reading
//! otherwise.

use std::io::{BufRead, IsTerminal, Write};
use std::sync::{Arc, Mutex};

use rustyline::completion::Completer;
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Context, Editor, Helper};
use tokio::sync::mpsc;
use tuff_core::ServerHandle;
use tuff_item::text::TextComponent;

/// Typed lines waiting for the previous command to finish.
const CONSOLE_QUEUE: usize = 16;
const PROMPT: &str = "> ";
const CONTEXT_MARKER: &str = "command.context.here";
const MAX_HINT: usize = 8;

/// Completions for the text before the cursor: the byte offset to replace
/// from and the candidates; `None` when the server is gone.
pub type CompletionSource = Arc<dyn Fn(&str) -> Option<(usize, Vec<String>)> + Send + Sync>;

/// Completions use UTF-16 offsets; the editor wants bytes.
pub fn byte_offset(line: &str, utf16: usize) -> usize {
    let mut units = 0;
    for (index, ch) in line.char_indices() {
        if units >= utf16 {
            return index;
        }
        units += ch.len_utf16();
    }
    line.len()
}

/// Asks the server for completions of `line`.
pub fn server_completions(
    runtime: &tokio::runtime::Handle,
    handle: &ServerHandle,
    line: &str,
) -> Option<(usize, Vec<String>)> {
    let found = runtime.block_on(handle.complete_command(line)).ok()?;
    Some((byte_offset(line, found.start), found.matches))
}

struct Editing {
    source: CompletionSource,
}

impl Completer for Editing {
    type Candidate = String;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<String>)> {
        let before = &line[..pos];
        Ok((self.source)(before).unwrap_or((pos, Vec::new())))
    }
}

impl Hinter for Editing {
    type Hint = String;
}
impl Highlighter for Editing {}
impl Validator for Editing {}
impl Helper for Editing {}

/// Starts reading the console and returns the typed lines. An interactive
/// terminal gets the line editor; anything else is read as plain lines
/// with no escape codes.
pub fn spawn_input(
    handle: ServerHandle,
    runtime: tokio::runtime::Handle,
) -> mpsc::Receiver<String> {
    let (lines, receiver) = mpsc::channel(CONSOLE_QUEUE);
    let interactive = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
    let spawned = std::thread::Builder::new()
        .name("console-stdin".into())
        .spawn(move || {
            if interactive && edit_lines(&lines, &handle, &runtime).is_ok() {
                return;
            }
            read_plain(std::io::stdin().lock(), &lines);
        });
    if let Err(err) = spawned {
        tuff_core::log::warn!("cannot read the console: {err}");
    }
    receiver
}

/// Sends each line of `reader` until it ends or the receiver is gone. A
/// blocking read cannot be cancelled, so this runs on its own thread.
pub fn read_plain(reader: impl BufRead, lines: &mpsc::Sender<String>) {
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if lines.blocking_send(line).is_err() {
            break;
        }
    }
}

fn edit_lines(
    lines: &mpsc::Sender<String>,
    handle: &ServerHandle,
    runtime: &tokio::runtime::Handle,
) -> rustyline::Result<()> {
    let mut editor: Editor<Editing, DefaultHistory> = Editor::new()?;
    let source: CompletionSource = {
        let handle = handle.clone();
        let runtime = runtime.clone();
        Arc::new(move |line| server_completions(&runtime, &handle, line))
    };
    editor.set_helper(Some(Editing { source }));
    let printer = Mutex::new(editor.create_external_printer()?);
    tuff_core::logging::set_terminal_sink(Some(Arc::new(move |text, to_stderr| {
        use rustyline::ExternalPrinter;
        let shown = printer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .print(format!("{text}\n"))
            .is_ok();
        if !shown {
            let _ = if to_stderr {
                writeln!(std::io::stderr(), "{text}")
            } else {
                writeln!(std::io::stdout(), "{text}")
            };
        }
    })));
    loop {
        match editor.readline(PROMPT) {
            Ok(line) => {
                if !line.trim().is_empty() {
                    let _ = editor.add_history_entry(line.as_str());
                }
                if lines.blocking_send(line).is_err() {
                    break;
                }
            }
            Err(ReadlineError::Interrupted) => {
                let _ = runtime.block_on(handle.shutdown());
                break;
            }
            Err(_) => break,
        }
    }
    tuff_core::logging::set_terminal_sink(None);
    Ok(())
}

/// Runs console commands until the lines end or the server stops.
pub async fn run(handle: ServerHandle, mut lines: mpsc::Receiver<String>) {
    while let Some(line) = lines.recv().await {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(feedback) = handle.run_command(line.clone()).await else {
            break;
        };
        let (rendered, cursor) = render_feedback(&feedback);
        for text in rendered {
            tuff_core::log::info!("{text}");
        }
        if let Some(cursor) = cursor
            && let Some(hint) = expected_hint(&handle, &line, cursor).await
        {
            tuff_core::log::info!("{hint}");
        }
    }
}

async fn expected_hint(handle: &ServerHandle, line: &str, cursor: usize) -> Option<String> {
    let trimmed = line.trim_start();
    let lead = line.len() - trimmed.len();
    let upto = line.get(..(lead + cursor.min(trimmed.len())))?;
    let found = handle.complete_command(upto).await.ok()?;
    hint_text(&found.matches)
}

/// "expected: a, b, c" for the options valid at the error, if any.
pub fn hint_text(matches: &[String]) -> Option<String> {
    if matches.is_empty() {
        return None;
    }
    let shown: Vec<&str> = matches.iter().take(MAX_HINT).map(String::as_str).collect();
    let more = if matches.len() > MAX_HINT {
        ", ..."
    } else {
        ""
    };
    Some(format!("  expected: {}{more}", shown.join(", ")))
}

fn is_context_line(line: &TextComponent) -> bool {
    line.extra
        .last()
        .is_some_and(|last| last.translate.as_deref() == Some(CONTEXT_MARKER))
}

/// Plain console lines for command feedback. A positioned error's context
/// line is shown with a caret line under the error position; the second
/// value is that position as a byte offset into the typed input.
pub fn render_feedback(feedback: &[TextComponent]) -> (Vec<String>, Option<usize>) {
    let mut out = Vec::new();
    let mut cursor = None;
    for line in feedback {
        if !is_context_line(line) {
            out.push(line.plain());
            continue;
        }
        let parts = &line.extra[..line.extra.len() - 1];
        let ellipsis = parts.first().is_some_and(|p| p.text == "...");
        let body = &parts[usize::from(ellipsis)..];
        let mut shown = String::new();
        let mut before = String::new();
        let mut underlined = 0;
        for part in body {
            if part.underlined == Some(true) {
                underlined += part.text.chars().count();
            } else if underlined == 0 {
                before.push_str(&part.text);
            }
            shown.push_str(&part.text);
        }
        let lead = if ellipsis { "..." } else { "" };
        out.push(format!("  {lead}{shown}"));
        let pad = lead.len() + before.chars().count();
        let tail = "~".repeat(underlined.saturating_sub(1));
        out.push(format!("  {}^{tail}", " ".repeat(pad)));
        cursor = Some(before.len());
    }
    (out, cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuff_core::command::CommandError;

    fn error_lines(input: &str, cursor: usize) -> Vec<TextComponent> {
        CommandError::simple("command.unknown.argument")
            .at_cursor(cursor)
            .display(input)
    }

    #[tuff_core::watchdog]
    #[test]
    fn byte_offsets_follow_utf16_units() {
        assert_eq!(byte_offset("say abc", 4), 4);
        assert_eq!(byte_offset("a\u{e9}b", 2), 3);
        assert_eq!(byte_offset("a\u{1f600}b", 3), 5);
        assert_eq!(byte_offset("ab", 9), 2);
    }

    #[tuff_core::watchdog]
    #[test]
    fn errors_get_a_caret_under_the_position() {
        let (lines, cursor) = render_feedback(&error_lines("gamemode creative extra", 18));
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert_eq!(lines[1], "  ... creative extra");
        assert_eq!(lines[2], "  ".to_owned() + &" ".repeat(13) + "^~~~~");
        assert_eq!(cursor, Some(10));
    }

    #[tuff_core::watchdog]
    #[test]
    fn short_inputs_and_unpositioned_errors_render() {
        let (lines, cursor) = render_feedback(&error_lines("abc", 3));
        assert_eq!(lines[1], "  abc");
        assert_eq!(lines[2], "     ^");
        assert_eq!(cursor, Some(3));
        let plain = CommandError::simple("command.unknown.argument").display("abc");
        let (lines, cursor) = render_feedback(&plain);
        assert_eq!(lines.len(), 1);
        assert_eq!(cursor, None);
    }

    #[tuff_core::watchdog]
    #[test]
    fn hints_list_options_and_truncate() {
        assert_eq!(hint_text(&[]), None);
        assert_eq!(
            hint_text(&["a".into(), "b".into()]).as_deref(),
            Some("  expected: a, b")
        );
        let many: Vec<String> = (0..10).map(|n| n.to_string()).collect();
        assert!(hint_text(&many).unwrap().ends_with(", ..."));
    }

    #[tuff_core::watchdog]
    #[test]
    fn completer_asks_the_source_with_text_before_the_cursor() {
        let editing = Editing {
            source: Arc::new(|line| {
                assert_eq!(line, "gam");
                Some((0, vec!["gamemode".into()]))
            }),
        };
        let history = DefaultHistory::new();
        let ctx = Context::new(&history);
        let (start, found) = editing.complete("gamemode", 3, &ctx).unwrap();
        assert_eq!((start, found), (0, vec!["gamemode".to_owned()]));
        let gone = Editing {
            source: Arc::new(|_| None),
        };
        assert_eq!(gone.complete("x", 1, &ctx).unwrap(), (1, Vec::new()));
    }

    #[tuff_core::watchdog]
    #[tokio::test]
    async fn plain_input_passes_lines_through_without_escapes() {
        let (tx, mut rx) = mpsc::channel(4);
        std::thread::spawn(move || read_plain(std::io::Cursor::new("say hi\nstop\n"), &tx))
            .join()
            .unwrap();
        assert_eq!(rx.recv().await.as_deref(), Some("say hi"));
        assert_eq!(rx.recv().await.as_deref(), Some("stop"));
        assert_eq!(rx.recv().await, None);
    }
}
