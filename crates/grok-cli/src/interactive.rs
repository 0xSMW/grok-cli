use crate::options::OutputFormat;
use crate::shell;
use anyhow::{Result, bail};

pub(crate) const DELETE_CONFIRMATION_PROMPT: &str =
    "Delete current conversation? type d to confirm delete ";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InteractiveCommand {
    pub(crate) name: String,
    pub(crate) has_slash: bool,
    pub(crate) remainder: String,
}

impl InteractiveCommand {
    pub(crate) fn is_exact(&self) -> bool {
        self.remainder.trim().is_empty()
    }

    pub(crate) fn arguments(&self) -> Result<Vec<String>> {
        shell::split_command_arguments(&self.remainder)
    }
}

pub(crate) fn parse_command(input: &str) -> Option<InteractiveCommand> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let has_slash = trimmed.starts_with('/');
    let command_text = if has_slash { &trimmed[1..] } else { trimmed };
    if command_text.is_empty() {
        return None;
    }

    let command_end = command_text
        .char_indices()
        .find_map(|(index, character)| character.is_whitespace().then_some(index))
        .unwrap_or(command_text.len());
    let command = InteractiveCommand {
        name: command_text[..command_end].to_lowercase(),
        has_slash,
        remainder: command_text[command_end..].trim().to_string(),
    };

    if has_slash || is_bare_command(&command) {
        Some(command)
    } else {
        None
    }
}

pub(crate) fn parse_skill_create_prompt(remainder: &str) -> Result<String> {
    let trimmed = remainder.trim();
    let command_end = trimmed
        .char_indices()
        .find_map(|(index, character)| character.is_whitespace().then_some(index))
        .unwrap_or(trimmed.len());

    let subcommand = trimmed[..command_end].to_lowercase();
    if subcommand != "create" {
        bail!("Usage: /skill create <prompt>");
    }

    let prompt = trimmed[command_end..].trim();
    if prompt.is_empty() {
        bail!("Usage: /skill create <prompt>");
    }
    Ok(prompt.to_string())
}

pub(crate) fn resolve_toggle(current: bool, args: &[&str], usage: &str) -> Result<bool> {
    if args.len() > 1 {
        bail!("Usage: {usage} [on|off]");
    }
    let Some(raw_value) = args.first().map(|value| value.to_lowercase()) else {
        return Ok(!current);
    };

    match raw_value.as_str() {
        "on" | "enable" | "enabled" | "true" => Ok(true),
        "off" | "disable" | "disabled" | "false" => Ok(false),
        _ => bail!("Usage: {usage} [on|off]"),
    }
}

pub(crate) fn resolve_output_raw(command: &str, args: &[&str], current_raw: bool) -> Result<bool> {
    match command {
        "format" => {
            if args.len() > 1 {
                bail!("Usage: /format [md|raw]");
            }
            let Some(raw_value) = args.first() else {
                return Ok(!current_raw);
            };
            let Some(format) = OutputFormat::resolve(raw_value) else {
                bail!("Usage: /format [md|raw]");
            };
            match format {
                OutputFormat::Json => {
                    bail!(
                        "JSON output is available from the CLI command line. Run a command with --json, for example: grok message --json <message>"
                    )
                }
                OutputFormat::Raw => Ok(true),
                OutputFormat::Markdown => Ok(false),
            }
        }
        "md" | "markdown" => {
            let markdown_enabled = resolve_toggle(!current_raw, args, &format!("/{command}"))?;
            Ok(!markdown_enabled)
        }
        "raw" => resolve_toggle(current_raw, args, "/raw"),
        _ => bail!("Usage: /format [md|raw]"),
    }
}

pub(crate) fn is_delete_confirmation(input: Option<&str>) -> bool {
    input
        .map(str::trim)
        .is_some_and(|value| value.eq_ignore_ascii_case("d"))
}

fn is_bare_command(command: &InteractiveCommand) -> bool {
    if matches!(
        command.name.as_str(),
        "exit" | "quit" | "help" | "new" | "resume" | "list" | "clear" | "cls" | "limits"
    ) {
        return command.is_exact();
    }

    let argument_count = command.remainder.split_whitespace().count();
    if matches!(command.name.as_str(), "model" | "models" | "mode" | "modes") {
        return command.is_exact() || argument_count == 1;
    }

    if matches!(
        command.name.as_str(),
        "reason" | "reasoning" | "private" | "stream" | "typeahead" | "md" | "markdown" | "raw"
    ) {
        if command.is_exact() {
            return true;
        }
        return argument_count == 1 && is_toggle_word(command.remainder.as_str());
    }

    if command.name == "format" {
        if command.is_exact() {
            return true;
        }
        return argument_count == 1 && OutputFormat::resolve(&command.remainder).is_some();
    }

    if command.name == "goal" {
        return true;
    }

    let Some(first_arg) = command
        .remainder
        .split_whitespace()
        .next()
        .map(|arg| arg.to_lowercase())
    else {
        return matches!(
            command.name.as_str(),
            "auth"
                | "tasks"
                | "skills"
                | "agents"
                | "workspaces"
                | "workspace"
                | "files"
                | "attach"
        );
    };

    match command.name.as_str() {
        "auth" if first_arg.starts_with('-') => true,
        "auth" => matches!(
            first_arg.as_str(),
            "generate"
                | "import"
                | "help"
                | "-h"
                | "--help"
                | "auto"
                | "safari"
                | "atlas"
                | "chrome"
                | "firefox"
                | "chromium"
                | "brave"
                | "edge"
                | "arc"
        ),
        "tasks" => matches!(
            first_arg.as_str(),
            "list"
                | "select"
                | "show"
                | "details"
                | "detail"
                | "results"
                | "result"
                | "create"
                | "archive"
                | "help"
                | "-h"
                | "--help"
                | "--json"
                | "--debug"
        ),
        "skills" => matches!(
            first_arg.as_str(),
            "list" | "mine" | "user" | "help" | "-h" | "--help" | "--json" | "--debug"
        ),
        "agents" => matches!(
            first_arg.as_str(),
            "list"
                | "show"
                | "view"
                | "edit"
                | "set"
                | "clear"
                | "help"
                | "-h"
                | "--help"
                | "--replace"
                | "--include-instructions"
                | "--show-instructions"
                | "--json"
                | "--debug"
        ),
        "workspaces" | "workspace" => matches!(
            first_arg.as_str(),
            "list"
                | "create"
                | "add-conversation"
                | "delete"
                | "remove"
                | "conversation"
                | "select"
                | "help"
                | "-h"
                | "--help"
                | "--json"
                | "--debug"
        ),
        "files" => matches!(
            first_arg.as_str(),
            "list"
                | "upload"
                | "delete"
                | "remove"
                | "help"
                | "-h"
                | "--help"
                | "--json"
                | "--debug"
        ),
        "attach" => matches!(first_arg.as_str(), "list" | "upload" | "clear"),
        _ => false,
    }
}

fn is_toggle_word(value: &str) -> bool {
    matches!(
        value.to_lowercase().as_str(),
        "on" | "off" | "enable" | "enabled" | "disable" | "disabled" | "true" | "false"
    )
}

#[cfg(test)]
mod tests {
    use super::{parse_command, parse_skill_create_prompt, resolve_output_raw, resolve_toggle};

    #[test]
    fn interactive_command_parser_preserves_swift_command_shape() {
        let command = parsed("/TASKS create --prompt \"draft update\"");
        assert_eq!(command.name, "tasks");
        assert!(command.has_slash);
        assert_eq!(command.remainder, "create --prompt \"draft update\"");
        assert_eq!(
            command_args(&command),
            vec!["create", "--prompt", "draft update"]
        );
    }

    #[test]
    fn interactive_command_parser_keeps_plain_unclosed_quotes_as_chat_like_swift() {
        assert_eq!(parse_command("hello \"unterminated"), None);

        let command = parsed("/tasks create --prompt \"unterminated");
        assert_eq!(
            command_arguments_error(&command),
            "Unclosed \" quote in command"
        );
    }

    #[test]
    fn interactive_command_parser_matches_swift_bare_boundaries() {
        assert_eq!(parsed("reasoning on").name, "reasoning");
        assert_eq!(parse_command("reasoning maybe"), None);
        assert_eq!(parsed("model expert").remainder, "expert");
        assert_eq!(parse_command("model this should remain chat"), None);
        assert_eq!(parsed("format json").remainder, "json");
        assert_eq!(parse_command("format xml"), None);
        assert_eq!(parsed("tasks create --prompt hi").name, "tasks");
        assert_eq!(parse_command("tasks bogus --prompt hi"), None);
    }

    #[test]
    fn skill_create_prompt_parser_uses_raw_remainder_like_swift() {
        assert_eq!(
            parse_skill_create_prompt("create summarize invoices").unwrap_or_default(),
            "summarize invoices"
        );
        assert_eq!(
            parse_skill_create_prompt("create \"summarize invoices\"").unwrap_or_default(),
            "\"summarize invoices\""
        );
        assert_eq!(
            skill_prompt_error("list skills"),
            "Usage: /skill create <prompt>"
        );
    }

    #[test]
    fn interactive_toggle_and_format_resolution_match_swift() {
        assert!(resolve_toggle(false, &[], "/stream").unwrap_or_default());
        assert!(resolve_toggle(false, &["enabled"], "/stream").unwrap_or_default());
        assert!(!resolve_toggle(true, &["disabled"], "/stream").unwrap_or(true));
        assert_eq!(
            toggle_error(false, &["maybe"], "/stream"),
            "Usage: /stream [on|off]"
        );

        assert!(resolve_output_raw("format", &["raw"], false).unwrap_or_default());
        assert!(!resolve_output_raw("format", &["md"], true).unwrap_or(true));
        assert!(!resolve_output_raw("raw", &["off"], true).unwrap_or(true));
        assert_eq!(
            output_raw_error("format", &["json"], false),
            "JSON output is available from the CLI command line. Run a command with --json, for example: grok message --json <message>"
        );
    }

    #[test]
    fn delete_confirmation_matches_swift_trimmed_d_only() {
        assert!(super::is_delete_confirmation(Some("d")));
        assert!(super::is_delete_confirmation(Some(" D \n")));
        assert!(!super::is_delete_confirmation(Some("delete")));
        assert!(!super::is_delete_confirmation(Some("yes")));
        assert!(!super::is_delete_confirmation(Some("")));
        assert!(!super::is_delete_confirmation(None));
    }

    fn parsed(input: &str) -> super::InteractiveCommand {
        match parse_command(input) {
            Some(command) => command,
            None => panic!("expected interactive command for input: {input}"),
        }
    }

    fn command_args(command: &super::InteractiveCommand) -> Vec<String> {
        match command.arguments() {
            Ok(args) => args,
            Err(error) => panic!("expected command arguments to parse, got {error}"),
        }
    }

    fn command_arguments_error(command: &super::InteractiveCommand) -> String {
        match command.arguments() {
            Ok(args) => panic!("expected command arguments to fail, got {args:?}"),
            Err(error) => error.to_string(),
        }
    }

    fn skill_prompt_error(remainder: &str) -> String {
        match parse_skill_create_prompt(remainder) {
            Ok(prompt) => panic!("expected skill prompt parsing to fail, got {prompt}"),
            Err(error) => error.to_string(),
        }
    }

    fn toggle_error(current: bool, args: &[&str], usage: &str) -> String {
        match resolve_toggle(current, args, usage) {
            Ok(value) => panic!("expected toggle resolution to fail, got {value}"),
            Err(error) => error.to_string(),
        }
    }

    fn output_raw_error(command: &str, args: &[&str], current_raw: bool) -> String {
        match resolve_output_raw(command, args, current_raw) {
            Ok(value) => panic!("expected output format resolution to fail, got {value}"),
            Err(error) => error.to_string(),
        }
    }
}
