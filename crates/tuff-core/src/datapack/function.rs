//! `.mcfunction` files: one command per line, `#` comments, `\` line
//! continuations and `$` macro lines with `$(name)` variables. Plain lines
//! are parsed once at load with the function permission level, so a
//! function with a bad command never loads; macro lines are parsed when a
//! call fills them in.

use crate::command::{CommandError, ParseContext, Parsed, Roots};
use std::sync::{Arc, Mutex};
use tuff_item::text::TextComponent;
use tuff_nbt::{Compound, Tag};
use tuff_storage::admin::PermissionLevel;

/// The longest command line a function may hold.
pub const MAX_LINE: usize = 2_000_000;

/// The permission level functions run with, and are compiled with unless
/// `function-permission-level` says otherwise.
pub const PERMISSION: PermissionLevel = PermissionLevel::Gamemasters;

fn parse_context() -> ParseContext {
    ParseContext {
        allow_selectors: true,
    }
}

/// A macro line split around its variables.
#[derive(Debug, Clone, PartialEq)]
pub struct Template {
    segments: Vec<String>,
    variables: Vec<String>,
}

fn valid_variable(name: &str) -> bool {
    name.chars().all(|c| c.is_alphanumeric() || c == '_')
}

impl Template {
    pub fn parse(input: &str) -> Result<Self, String> {
        let mut segments = Vec::new();
        let mut variables = Vec::new();
        let mut start = 0;
        let mut search = 0;
        while let Some(found) = input[search..].find('$') {
            let index = search + found;
            if !input[index + 1..].starts_with('(') {
                search = index + 1;
                continue;
            }
            segments.push(input[start..index].to_owned());
            let Some(close) = input[index + 2..].find(')') else {
                return Err("Unterminated macro variable".to_owned());
            };
            let variable = &input[index + 2..index + 2 + close];
            if !valid_variable(variable) {
                return Err(format!("Invalid macro variable name '{variable}'"));
            }
            variables.push(variable.to_owned());
            start = index + 2 + close + 1;
            search = start;
        }
        if start == 0 {
            return Err("No variables in macro".to_owned());
        }
        if start != input.len() {
            segments.push(input[start..].to_owned());
        }
        Ok(Self {
            segments,
            variables,
        })
    }

    pub fn variables(&self) -> &[String] {
        &self.variables
    }

    fn substitute(&self, values: &[&str]) -> Result<String, String> {
        let mut out = String::new();
        for (segment, value) in self.segments.iter().zip(values) {
            out.push_str(segment);
            out.push_str(value);
            check_length(&out)?;
        }
        if self.segments.len() > self.variables.len()
            && let Some(last) = self.segments.last()
        {
            out.push_str(last);
        }
        check_length(&out)?;
        Ok(out)
    }
}

fn check_length(line: &str) -> Result<(), String> {
    if line.chars().count() > MAX_LINE {
        let shown: String = line.chars().take(512).collect();
        return Err(format!(
            "Command too long: {} characters, contents: {shown}...",
            line.chars().count()
        ));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub enum Line {
    Command { input: String, parsed: Parsed },
    Macro { template: Template, line: usize },
}

/// A loaded function.
#[derive(Debug, Clone)]
pub struct Function {
    pub id: String,
    pub lines: Vec<Line>,
    /// Macro variables in first-use order; empty for a plain function.
    pub parameters: Vec<String>,
    /// The level its lines (macro lines too) are parsed at.
    pub permission: PermissionLevel,
    instantiated: Arc<Mutex<Vec<Instantiation>>>,
}

type Instantiation = (Vec<String>, Filled);
type Filled = Arc<[Option<(String, Parsed)>]>;

const INSTANTIATION_CACHE: usize = 8;

/// One call's filled-in macro lines (`None` for a compiled line).
#[derive(Debug, Default)]
pub struct Call {
    filled: Option<Filled>,
}

fn error_text(error: &CommandError, input: &str) -> String {
    error
        .display(input)
        .iter()
        .map(TextComponent::plain)
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_command(
    roots: &Roots,
    input: &str,
    permission: PermissionLevel,
) -> Result<Parsed, String> {
    let parsed = roots.parse(input, permission, parse_context());
    match parsed.error(input) {
        Some(error) => Err(error_text(&error, input)),
        None => Ok(parsed),
    }
}

/// A macro argument as it is pasted into the command.
fn stringify(tag: &Tag) -> String {
    match tag {
        Tag::Float(v) => decimal(f64::from(*v)),
        Tag::Double(v) => decimal(*v),
        Tag::Byte(v) => v.to_string(),
        Tag::Short(v) => v.to_string(),
        Tag::Long(v) => v.to_string(),
        Tag::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A number with at most 15 fraction digits and no exponent.
fn decimal(value: f64) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    let text = format!("{value:.15}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text == "-0" {
        "-0".to_owned()
    } else {
        text.to_owned()
    }
}

impl Function {
    /// Compiles `source`, the file's text, at [`PERMISSION`].
    pub fn compile(id: &str, source: &str, roots: &Roots) -> Result<Self, String> {
        Self::compile_at(id, source, roots, PERMISSION)
    }

    /// Compiles `source` with commands up to `permission` allowed.
    pub fn compile_at(
        id: &str,
        source: &str,
        roots: &Roots,
        permission: PermissionLevel,
    ) -> Result<Self, String> {
        let raw: Vec<&str> = source.lines().collect();
        let mut lines = Vec::new();
        let mut parameters: Vec<String> = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let number = i + 1;
            let mut line = raw[i].trim().to_owned();
            if line.ends_with('\\') {
                loop {
                    i += 1;
                    if i == raw.len() {
                        return Err("Line continuation at end of file".to_owned());
                    }
                    line.pop();
                    line.push_str(raw[i].trim());
                    check_length(&line)?;
                    if !line.ends_with('\\') {
                        break;
                    }
                }
            }
            check_length(&line)?;
            i += 1;
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(rest) = line.strip_prefix('/') {
                if rest.starts_with('/') {
                    return Err(format!(
                        "Unknown or invalid command '{line}' on line {number} (if you intended to make a comment, use '#' not '//')"
                    ));
                }
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '+'))
                    .collect();
                return Err(format!(
                    "Unknown or invalid command '{line}' on line {number} (did you mean '{name}'? Do not use a preceding forwards slash.)"
                ));
            }
            if let Some(rest) = line.strip_prefix('$') {
                let template = Template::parse(rest).map_err(|err| {
                    format!("Can't parse function line {number}: '{rest}': {err}")
                })?;
                for variable in template.variables() {
                    if !parameters.contains(variable) {
                        parameters.push(variable.clone());
                    }
                }
                lines.push(Line::Macro {
                    template,
                    line: number,
                });
                continue;
            }
            let parsed = parse_command(roots, &line, permission)
                .map_err(|err| format!("Whilst parsing command on line {number}: {err}"))?;
            lines.push(Line::Command {
                input: line,
                parsed,
            });
        }
        Ok(Self {
            id: id.to_owned(),
            lines,
            parameters,
            permission,
            instantiated: Arc::default(),
        })
    }

    pub fn is_macro(&self) -> bool {
        !self.parameters.is_empty()
    }

    /// One call's lines, with macro lines filled in from `arguments`. The
    /// error is the reason the game shows.
    pub fn prepare(
        &self,
        arguments: Option<&Compound>,
        roots: &Roots,
    ) -> Result<Call, TextComponent> {
        let id = || TextComponent::new(self.id.clone());
        if !self.is_macro() {
            return Ok(Call::default());
        }
        let Some(arguments) = arguments else {
            return Err(TextComponent::translate(
                "commands.function.error.missing_arguments",
                [id()],
            ));
        };
        let mut values = Vec::with_capacity(self.parameters.len());
        for parameter in &self.parameters {
            let Some(value) = arguments.get(parameter) else {
                return Err(TextComponent::translate(
                    "commands.function.error.missing_argument",
                    [id(), TextComponent::new(parameter.clone())],
                ));
            };
            values.push(stringify(value));
        }
        if let Ok(mut cache) = self.instantiated.lock()
            && let Some(at) = cache.iter().position(|(key, _)| *key == values)
        {
            let hit = cache.remove(at);
            let filled = hit.1.clone();
            cache.push(hit);
            return Ok(Call {
                filled: Some(filled),
            });
        }
        let mut filled = Vec::with_capacity(self.lines.len());
        for line in &self.lines {
            let Line::Macro { template, .. } = line else {
                filled.push(None);
                continue;
            };
            let picked: Vec<&str> = template
                .variables()
                .iter()
                .map(|variable| {
                    let index = self
                        .parameters
                        .iter()
                        .position(|p| p == variable)
                        .unwrap_or(0);
                    values[index].as_str()
                })
                .collect();
            let command = template.substitute(&picked).map_err(TextComponent::new)?;
            let parsed = parse_command(roots, &command, self.permission).map_err(|err| {
                TextComponent::translate(
                    "commands.function.error.parse",
                    [
                        id(),
                        TextComponent::new(command.clone()),
                        TextComponent::new(err),
                    ],
                )
            })?;
            filled.push(Some((command, parsed)));
        }
        let filled: Filled = filled.into();
        if let Ok(mut cache) = self.instantiated.lock() {
            if cache.len() >= INSTANTIATION_CACHE {
                cache.remove(0);
            }
            cache.push((values, filled.clone()));
        }
        Ok(Call {
            filled: Some(filled),
        })
    }

    /// The text and parse of line `index` in `call`.
    pub fn command<'a>(&'a self, call: &'a Call, index: usize) -> Option<(&'a str, &'a Parsed)> {
        if let Some(Some((input, parsed))) = call.filled.as_deref().and_then(|f| f.get(index)) {
            return Some((input, parsed));
        }
        match self.lines.get(index)? {
            Line::Command { input, parsed } => Some((input, parsed)),
            Line::Macro { .. } => None,
        }
    }

    /// Every command of `call`, in order.
    pub fn commands<'a>(&'a self, call: &'a Call) -> impl Iterator<Item = (&'a str, &'a Parsed)> {
        (0..self.lines.len()).filter_map(move |index| self.command(call, index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::RootCommand;

    fn roots() -> Roots {
        Roots::default().with_commands(
            ["say", "scoreboard", "function", "execute", "kill", "stop"]
                .map(|name| RootCommand::vanilla(name, &[])),
        )
    }

    #[tuff_core::watchdog]
    #[test]
    fn templates_split_around_variables() {
        let template = Template::parse("say $(a) and $(b_2)!").unwrap();
        assert_eq!(template.variables(), ["a", "b_2"]);
        assert_eq!(template.substitute(&["x", "y"]).unwrap(), "say x and y!");
        assert_eq!(
            Template::parse("$(only)")
                .unwrap()
                .substitute(&["v"])
                .unwrap(),
            "v"
        );
        assert!(Template::parse("say $ cost $5").is_err());
        assert!(Template::parse("say $(open").is_err());
        assert!(Template::parse("say $(bad name)").is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn plain_lines_parse_at_load_and_comments_are_skipped() {
        let source = "# a comment\n\n  say one \nsay \\\n  two\n";
        let function = Function::compile("t:f", source, &roots()).unwrap();
        assert!(!function.is_macro());
        let call = function.prepare(None, &roots()).unwrap();
        let inputs: Vec<&str> = function.commands(&call).map(|(input, _)| input).collect();
        assert_eq!(inputs, ["say one", "say two"]);
    }

    #[tuff_core::watchdog]
    #[test]
    fn bad_lines_stop_the_function_loading() {
        let roots = roots();
        let err = Function::compile("t:f", "say ok\nnotacommand", &roots).unwrap_err();
        assert!(err.starts_with("Whilst parsing command on line 2"), "{err}");
        let err = Function::compile("t:f", "/say hi", &roots).unwrap_err();
        assert!(err.contains("did you mean 'say'"), "{err}");
        let err = Function::compile("t:f", "// hi", &roots).unwrap_err();
        assert!(err.contains("use '#' not '//'"), "{err}");
        let err = Function::compile("t:f", "say \\", &roots).unwrap_err();
        assert_eq!(err, "Line continuation at end of file");
    }

    #[tuff_core::watchdog]
    #[test]
    fn the_compile_level_decides_which_commands_load() {
        let roots = roots();
        let err = Function::compile("t:f", "stop", &roots).unwrap_err();
        assert!(err.starts_with("Whilst parsing command on line 1"), "{err}");
        let owners = PermissionLevel::Owners;
        Function::compile_at("t:f", "stop", &roots, owners).unwrap();
        let low = PermissionLevel::Moderators;
        assert!(Function::compile_at("t:f", "kill @e", &roots, low).is_err());
        let macro_line = Function::compile_at("t:m", "$stop$(x)", &roots, owners).unwrap();
        let arguments = Compound::new().with("x", "");
        assert!(macro_line.prepare(Some(&arguments), &roots).is_ok());
        let macro_line = Function::compile("t:m", "$stop$(x)", &roots).unwrap();
        assert!(macro_line.prepare(Some(&arguments), &roots).is_err());
    }

    #[tuff_core::watchdog]
    #[test]
    fn macros_fill_in_arguments_when_called() {
        let roots = roots();
        let function =
            Function::compile("t:m", "say before\n$say $(word) $(n) $(f)\n", &roots).unwrap();
        assert_eq!(function.parameters, ["word", "n", "f"]);
        let missing = function.prepare(None, &roots).unwrap_err();
        assert_eq!(
            missing.translate.as_deref(),
            Some("commands.function.error.missing_arguments")
        );
        let partial = Compound::new().with("word", "hi");
        let missing = function.prepare(Some(&partial), &roots).unwrap_err();
        assert_eq!(
            missing.translate.as_deref(),
            Some("commands.function.error.missing_argument")
        );
        let full = Compound::new()
            .with("word", "hi")
            .with("n", Tag::Byte(3))
            .with("f", Tag::Double(1.25));
        let call = function.prepare(Some(&full), &roots).unwrap();
        let inputs: Vec<&str> = function.commands(&call).map(|(input, _)| input).collect();
        assert_eq!(inputs, ["say before", "say hi 3 1.25"]);
        let again = function.prepare(Some(&full), &roots).unwrap();
        assert!(Arc::ptr_eq(
            call.filled.as_ref().unwrap(),
            again.filled.as_ref().unwrap()
        ));
        for n in 0..INSTANTIATION_CACHE + 2 {
            let args = full.clone().with("n", Tag::Byte(n as i8));
            function.prepare(Some(&args), &roots).unwrap();
        }
        assert_eq!(
            function.instantiated.lock().unwrap().len(),
            INSTANTIATION_CACHE
        );
        let bad = Function::compile("t:m", "$kill $(target)", &roots).unwrap();
        let err = bad
            .prepare(Some(&Compound::new().with("target", "@q")), &roots)
            .err()
            .unwrap();
        assert_eq!(
            err.translate.as_deref(),
            Some("commands.function.error.parse")
        );
    }

    #[tuff_core::watchdog]
    #[test]
    fn macro_numbers_print_without_exponents() {
        assert_eq!(decimal(1.0), "1");
        assert_eq!(decimal(0.5), "0.5");
        assert_eq!(decimal(1e20), "100000000000000000000");
        assert_eq!(decimal(f64::from(0.1f32)), "0.100000001490116");
        assert_eq!(stringify(&Tag::Long(5)), "5");
        assert_eq!(stringify(&Tag::Int(-2)), "-2");
        assert_eq!(stringify(&Tag::List(vec![Tag::Int(1)])), "[1]");
    }
}
