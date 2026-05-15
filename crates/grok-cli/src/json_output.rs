use serde::Serialize;
use serde_json::{Value, json};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliJsonError {
    pub code: String,
    pub message: String,
    pub exit_code: i32,
    pub recoverable: bool,
    pub raw_message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CliJsonResult {
    pub schema: &'static str,
    pub ok: bool,
    pub command: String,
    pub subcommand: Option<String>,
    pub category: String,
    pub data: Option<Value>,
    pub error: Option<CliJsonError>,
    pub meta: Value,
}

impl CliJsonResult {
    pub fn ok(
        command: impl Into<String>,
        subcommand: Option<String>,
        category: impl Into<String>,
        data: Value,
        meta: Value,
    ) -> Self {
        Self {
            schema: "grok.cli.result.v1",
            ok: true,
            command: command.into(),
            subcommand,
            category: category.into(),
            data: Some(data),
            error: None,
            meta,
        }
    }

    pub fn error(
        command: impl Into<String>,
        subcommand: Option<String>,
        message: impl Into<String>,
        code: impl Into<String>,
        exit_code: i32,
        recoverable: bool,
        raw_message: Option<String>,
    ) -> Self {
        Self {
            schema: "grok.cli.result.v1",
            ok: false,
            command: command.into(),
            subcommand,
            category: "error".to_string(),
            data: None,
            error: Some(CliJsonError {
                code: code.into(),
                message: message.into(),
                exit_code,
                recoverable,
                raw_message,
            }),
            meta: default_json_meta(false, Vec::new()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct CliJsonEvent {
    pub schema: &'static str,
    pub sequence: usize,
    pub event: String,
    pub data: Value,
}

impl CliJsonEvent {
    pub fn new(sequence: usize, event: impl Into<String>, data: Value) -> Self {
        Self {
            schema: "grok.cli.event.v1",
            sequence,
            event: event.into(),
            data,
        }
    }
}

pub fn default_json_meta(debug: bool, warnings: Vec<String>) -> Value {
    json!({
        "format": "json",
        "version": "1",
        "debug": debug,
        "warnings": warnings
    })
}

pub fn is_json_requested(args: &[impl AsRef<str>]) -> bool {
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_ref();
        if arg == "--json" {
            return true;
        }
        if let Some(value) = arg.strip_prefix("--format=") {
            return value.eq_ignore_ascii_case("json");
        }
        if arg == "--format"
            && index + 1 < args.len()
            && args[index + 1].as_ref().eq_ignore_ascii_case("json")
        {
            return true;
        }
        index += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{CliJsonResult, default_json_meta, is_json_requested};

    #[test]
    fn detects_json_request_like_swift_router() {
        assert!(is_json_requested(&["--json"]));
        assert!(is_json_requested(&["--format", "json"]));
        assert!(is_json_requested(&["--format=JSON"]));
        assert!(!is_json_requested(&["--format", "raw"]));
    }

    #[test]
    fn result_uses_stable_schema_and_meta() -> serde_json::Result<()> {
        let result = CliJsonResult::ok(
            "models",
            None,
            "model_list",
            json!({"modes": []}),
            default_json_meta(true, vec!["warning".to_string()]),
        );

        let encoded = serde_json::to_value(result)?;
        assert_eq!(encoded["schema"], "grok.cli.result.v1");
        assert_eq!(encoded["ok"], true);
        assert_eq!(encoded["meta"]["format"], "json");
        assert_eq!(encoded["meta"]["version"], "1");
        assert_eq!(encoded["meta"]["debug"], true);
        assert_eq!(encoded["meta"]["warnings"][0], "warning");
        Ok(())
    }
}
