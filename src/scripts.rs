//! Runs `--execute` script files through the console's `/tuff execute`.

use tuff_core::ServerHandle;
use tuff_item::text::TextComponent;

/// What one script run printed and whether every command succeeded.
#[derive(Debug, PartialEq)]
pub struct ScriptReport {
    pub file: String,
    pub feedback: Vec<String>,
    pub ok: bool,
}

fn quoted(file: &str) -> String {
    format!("tuff execute \"{file}\" as console")
}

fn succeeded(feedback: &[TextComponent]) -> bool {
    feedback
        .last()
        .is_some_and(|line| line.plain().ends_with("(0 failed)"))
}

/// Runs each file in order through the same code as `/tuff execute <file>
/// as console`, stopping at the first server stop. A file that cannot be
/// read, or any failing line, makes its report not `ok`; later files still
/// run.
pub async fn run_scripts(
    handle: &ServerHandle,
    files: &[String],
) -> Result<Vec<ScriptReport>, tuff_core::server::ServerStopped> {
    let mut reports = Vec::new();
    for file in files {
        let feedback = handle.run_operator_command(quoted(file)).await?;
        reports.push(ScriptReport {
            file: file.clone(),
            ok: succeeded(&feedback),
            feedback: feedback.iter().map(TextComponent::plain).collect(),
        });
    }
    Ok(reports)
}
