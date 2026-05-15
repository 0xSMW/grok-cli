use crate::options::OutputFormat;
use grok_client::GrokMode;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelOptionResult {
    pub mode: Option<GrokMode>,
    pub consumed_next: bool,
    pub missing_value: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutputFormatOptionResult {
    pub format: Option<OutputFormat>,
    pub consumed_next: bool,
    pub missing_value: bool,
    pub invalid_value: Option<String>,
}

pub fn apply_model_option(arg: &str, next_value: Option<&str>) -> ModelOptionResult {
    if arg == "--model" || arg == "--mode" {
        let Some(next_value) = next_value else {
            return missing_model_value();
        };
        if next_value.trim().is_empty() || next_value.starts_with("--") {
            return missing_model_value();
        }
        return ModelOptionResult {
            mode: Some(GrokMode::resolve(Some(next_value))),
            consumed_next: true,
            missing_value: false,
        };
    }

    if let Some(value) = arg
        .strip_prefix("--model=")
        .or_else(|| arg.strip_prefix("--mode="))
    {
        return ModelOptionResult {
            mode: (!value.is_empty()).then(|| GrokMode::resolve(Some(value))),
            consumed_next: false,
            missing_value: value.is_empty(),
        };
    }

    ModelOptionResult {
        mode: None,
        consumed_next: false,
        missing_value: false,
    }
}

pub fn apply_output_format_option(arg: &str, next_value: Option<&str>) -> OutputFormatOptionResult {
    match arg {
        "--json" => {
            return output_format(OutputFormat::Json, false);
        }
        "--raw" => {
            return output_format(OutputFormat::Raw, false);
        }
        "--markdown" | "-m" => {
            return output_format(OutputFormat::Markdown, false);
        }
        "--format" => {
            let Some(next_value) = next_value else {
                return missing_output_format_value();
            };
            if next_value.trim().is_empty() {
                return missing_output_format_value();
            }
            return match OutputFormat::resolve(next_value) {
                Some(format) => output_format(format, true),
                None => invalid_output_format(next_value),
            };
        }
        _ => {}
    }

    if let Some(value) = arg.strip_prefix("--format=") {
        if value.is_empty() {
            return missing_output_format_value();
        }
        return match OutputFormat::resolve(value) {
            Some(format) => output_format(format, false),
            None => invalid_output_format(value),
        };
    }

    OutputFormatOptionResult {
        format: None,
        consumed_next: false,
        missing_value: false,
        invalid_value: None,
    }
}

fn missing_model_value() -> ModelOptionResult {
    ModelOptionResult {
        mode: None,
        consumed_next: false,
        missing_value: true,
    }
}

fn output_format(format: OutputFormat, consumed_next: bool) -> OutputFormatOptionResult {
    OutputFormatOptionResult {
        format: Some(format),
        consumed_next,
        missing_value: false,
        invalid_value: None,
    }
}

fn missing_output_format_value() -> OutputFormatOptionResult {
    OutputFormatOptionResult {
        format: None,
        consumed_next: false,
        missing_value: true,
        invalid_value: None,
    }
}

fn invalid_output_format(value: &str) -> OutputFormatOptionResult {
    OutputFormatOptionResult {
        format: None,
        consumed_next: false,
        missing_value: false,
        invalid_value: Some(value.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{apply_model_option, apply_output_format_option};
    use crate::options::OutputFormat;

    #[test]
    fn model_option_parser_matches_swift_value_shapes() {
        let separate = apply_model_option("--model", Some("expert"));
        assert_eq!(
            separate.mode.as_ref().map(|mode| mode.id.as_str()),
            Some("expert")
        );
        assert!(separate.consumed_next);
        assert!(!separate.missing_value);

        let alias = apply_model_option("--mode=4.3", None);
        assert_eq!(
            alias.mode.as_ref().map(|mode| mode.id.as_str()),
            Some("grok-420-computer-use-sa")
        );
        assert!(!alias.consumed_next);
        assert!(!alias.missing_value);

        assert!(apply_model_option("--model", None).missing_value);
        assert!(apply_model_option("--model", Some("   ")).missing_value);
        assert!(apply_model_option("--mode", Some("--debug")).missing_value);
        assert!(apply_model_option("--model=", None).missing_value);
        assert_eq!(apply_model_option("--other", Some("fast")).mode, None);
    }

    #[test]
    fn output_format_option_parser_matches_swift_value_shapes() {
        assert_eq!(
            apply_output_format_option("--json", None).format,
            Some(OutputFormat::Json)
        );
        assert_eq!(
            apply_output_format_option("--raw", None).format,
            Some(OutputFormat::Raw)
        );
        assert_eq!(
            apply_output_format_option("-m", None).format,
            Some(OutputFormat::Markdown)
        );

        let separate = apply_output_format_option("--format", Some("plain"));
        assert_eq!(separate.format, Some(OutputFormat::Raw));
        assert!(separate.consumed_next);

        let inline = apply_output_format_option("--format=json", None);
        assert_eq!(inline.format, Some(OutputFormat::Json));
        assert!(!inline.consumed_next);

        assert!(apply_output_format_option("--format", None).missing_value);
        assert!(apply_output_format_option("--format=", None).missing_value);
        assert_eq!(
            apply_output_format_option("--format", Some("xml")).invalid_value,
            Some("xml".to_string())
        );
    }
}
