use std::collections::BTreeSet;

pub fn is_help_argument(value: &str) -> bool {
    matches!(value.to_lowercase().as_str(), "help" | "-h" | "--help")
}

pub fn recognized_top_level_commands() -> BTreeSet<&'static str> {
    BTreeSet::from([
        "chat",
        "message",
        "auth",
        "help",
        "list",
        "models",
        "modes",
        "agents",
        "tasks",
        "skills",
        "workspaces",
        "workspace",
        "files",
        "transcribe",
        "test",
    ])
}

pub fn disabled_top_level_commands() -> BTreeSet<&'static str> {
    BTreeSet::from(["code"])
}

pub fn normalized_top_level_arguments(arguments: &[String]) -> Vec<String> {
    let mut leading_options = Vec::new();
    let mut index = 0;

    while index < arguments.len() {
        let arg = &arguments[index];

        if is_top_level_flag_option(arg) || is_inline_top_level_value_option(arg) {
            leading_options.push(arg.clone());
            index += 1;
            continue;
        }

        if is_top_level_value_option(arg) && index + 1 < arguments.len() {
            leading_options.push(arg.clone());
            leading_options.push(arguments[index + 1].clone());
            index += 2;
            continue;
        }

        break;
    }

    if leading_options.is_empty() || index >= arguments.len() {
        return arguments.to_vec();
    }

    let candidate = arguments[index].to_lowercase();
    if !(recognized_top_level_commands().contains(candidate.as_str())
        || disabled_top_level_commands().contains(candidate.as_str())
        || is_help_argument(&candidate))
    {
        return arguments.to_vec();
    }

    let mut normalized = vec![arguments[index].clone()];
    normalized.extend(leading_options);
    normalized.extend(arguments.iter().skip(index + 1).cloned());
    normalized
}

fn is_top_level_flag_option(arg: &str) -> bool {
    matches!(
        arg,
        "--reasoning"
            | "--deep-search"
            | "--no-search"
            | "--markdown"
            | "-m"
            | "--raw"
            | "--json"
            | "--debug"
            | "--quiet"
            | "--no-custom-instructions"
            | "--private"
            | "--stream"
    )
}

fn is_top_level_value_option(arg: &str) -> bool {
    matches!(
        arg,
        "--format"
            | "--model"
            | "--mode"
            | "--audio"
            | "--audio-format"
            | "--refinement-level"
            | "--prompt-file"
            | "--file"
            | "--upload"
            | "--attach"
    )
}

fn is_inline_top_level_value_option(arg: &str) -> bool {
    [
        "--format=",
        "--model=",
        "--mode=",
        "--audio=",
        "--audio-format=",
        "--refinement-level=",
        "--prompt-file=",
        "--file=",
        "--upload=",
        "--attach=",
    ]
    .iter()
    .any(|prefix| arg.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::normalized_top_level_arguments;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn moves_leading_global_flags_after_command_like_swift_router() {
        assert_eq!(
            normalized_top_level_arguments(&args(&["--json", "--format", "raw", "models"])),
            args(&["models", "--json", "--format", "raw"])
        );
    }

    #[test]
    fn leaves_message_text_alone_when_no_command_is_found() {
        assert_eq!(
            normalized_top_level_arguments(&args(&["--json", "hello", "world"])),
            args(&["--json", "hello", "world"])
        );
    }
}
