use anyhow::Result;

pub fn split_command_arguments(input: &str) -> Result<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut current_started = false;
    let mut quote = None;
    let mut escaping = false;

    for character in input.chars() {
        if escaping {
            current.push(character);
            current_started = true;
            escaping = false;
            continue;
        }

        if character == '\\' {
            escaping = true;
            current_started = true;
            continue;
        }

        if let Some(active_quote) = quote {
            if character == active_quote {
                quote = None;
            } else {
                current.push(character);
                current_started = true;
            }
            continue;
        }

        if character == '"' || character == '\'' {
            quote = Some(character);
            current_started = true;
            continue;
        }

        if character.is_whitespace() {
            if current_started {
                args.push(std::mem::take(&mut current));
                current_started = false;
            }
            continue;
        }

        current.push(character);
        current_started = true;
    }

    if escaping {
        current.push('\\');
    }

    if let Some(active_quote) = quote {
        anyhow::bail!("Unclosed {active_quote} quote in command");
    }

    if current_started {
        args.push(current);
    }

    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::split_command_arguments;
    use anyhow::Result;

    #[test]
    fn split_command_arguments_preserves_swift_quotes_and_whitespace() -> Result<()> {
        assert_eq!(
            split_command_arguments(
                r#"tasks create --prompt "quoted prompt value" --name 'Bare Task'"#
            )?,
            vec![
                "tasks",
                "create",
                "--prompt",
                "quoted prompt value",
                "--name",
                "Bare Task"
            ]
        );
        Ok(())
    }

    #[test]
    fn split_command_arguments_keeps_empty_quoted_arguments_like_swift() -> Result<()> {
        assert_eq!(
            split_command_arguments(r#"cmd "" '' end"#)?,
            vec!["cmd", "", "", "end"]
        );
        Ok(())
    }

    #[test]
    fn split_command_arguments_treats_backslash_as_general_escape_like_swift() -> Result<()> {
        assert_eq!(
            split_command_arguments(r#"cmd escaped\ value "quoted \"value\"" trailing\"#)?,
            vec!["cmd", "escaped value", r#"quoted "value""#, r#"trailing\"#]
        );
        Ok(())
    }

    #[test]
    fn split_command_arguments_allows_quotes_inside_opposite_quote_like_swift() -> Result<()> {
        assert_eq!(
            split_command_arguments(r#"cmd "it's fine" 'say "hi"'"#)?,
            vec!["cmd", "it's fine", r#"say "hi""#]
        );
        Ok(())
    }

    #[test]
    fn split_command_arguments_reports_unclosed_quote_like_swift() {
        match split_command_arguments(r#"cmd "missing"#) {
            Ok(arguments) => panic!("split should have failed, got {arguments:?}"),
            Err(error) => assert_eq!(error.to_string(), r#"Unclosed " quote in command"#),
        }
    }
}
