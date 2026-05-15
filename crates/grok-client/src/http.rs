use serde_json::Value;

use crate::{GrokError, GrokMode, Result};

pub fn validate_http_response(status_code: u16, data: &[u8], mode_id: Option<&str>) -> Result<()> {
    if (200..=299).contains(&status_code) {
        return Ok(());
    }

    match status_code {
        401 => Err(GrokError::Unauthorized),
        403 => {
            let body_message = http_error_body_message(data);
            if response_body_indicates_authentication_failure(body_message.as_deref()) {
                return Err(GrokError::Unauthorized);
            }
            Err(GrokError::AccessDenied(access_denied_message(
                body_message.as_deref(),
                mode_id,
            )))
        }
        404 => Err(GrokError::NotFound("Not found".to_string())),
        _ => {
            let body = String::from_utf8_lossy(data)
                .trim()
                .chars()
                .take(600)
                .collect::<String>();
            let suffix = if body.is_empty() {
                String::new()
            } else {
                format!(": {body}")
            };
            Err(GrokError::Api(format!("HTTP Error: {status_code}{suffix}")))
        }
    }
}

pub fn http_error_body_message(data: &[u8]) -> Option<String> {
    if data.is_empty() {
        return None;
    }

    if let Ok(json) = serde_json::from_slice::<Value>(data) {
        if let Some(dictionary) = json.as_object() {
            if let Some(error) = dictionary.get("error") {
                return trimmed_message(Some(describe_api_error(error)));
            }
            for key in ["message", "description", "detail"] {
                if let Some(message) = dictionary.get(key).and_then(Value::as_str) {
                    return trimmed_message(Some(message.to_string()));
                }
            }
        }
        return trimmed_message(Some(json.to_string()));
    }

    trimmed_message(Some(String::from_utf8_lossy(data).to_string()))
}

pub fn response_body_indicates_authentication_failure(message: Option<&str>) -> bool {
    let Some(message) = message else {
        return false;
    };

    let normalized = message.to_lowercase();
    normalized.contains("unauthorized")
        || normalized.contains("unauthenticated")
        || normalized.contains("not authenticated")
        || normalized.contains("authentication required")
        || normalized.contains("login required")
        || normalized.contains("log in")
        || (normalized.contains("cookie")
            && (normalized.contains("invalid") || normalized.contains("expired")))
        || normalized.contains("csrf")
        || normalized.contains("sso")
}

pub fn access_denied_message(body_message: Option<&str>, mode_id: Option<&str>) -> String {
    let subject = if let Some(mode_id) = mode_id {
        let mode = GrokMode::resolve(Some(mode_id));
        format!("{} ({})", mode.display_name, mode.id)
    } else {
        "this request".to_string()
    };

    let mut message = format!("Grok denied access to {subject}.");
    if let Some(body_message) =
        body_message.filter(|message| !is_generic_forbidden_message(message))
    {
        message.push(' ');
        message.push_str(body_message);
    } else {
        message.push_str(" The selected model or feature may not be available to your account.");
    }
    message.push_str(
        " Switch models with `/model` or pass `--model fast`, `--model expert`, or `--model auto`.",
    );
    message
}

fn describe_api_error(value: &Value) -> String {
    if let Some(message) = value.as_str() {
        return message.to_string();
    }

    if let Some(dictionary) = value.as_object() {
        for key in ["message", "error", "description"] {
            if let Some(message) = dictionary.get(key).and_then(Value::as_str) {
                return message.to_string();
            }
        }
        return Value::Object(dictionary.clone()).to_string();
    }

    value.to_string()
}

fn trimmed_message(message: Option<String>) -> Option<String> {
    let trimmed = message
        .unwrap_or_default()
        .trim()
        .chars()
        .take(600)
        .collect::<String>();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn is_generic_forbidden_message(message: &str) -> bool {
    matches!(
        message.trim().to_lowercase().as_str(),
        "forbidden"
            | "access denied"
            | "{\"error\":\"forbidden\"}"
            | "{\"error\":\"access denied\"}"
    )
}

#[cfg(test)]
mod tests {
    use crate::{
        GrokError, http_error_body_message, response_body_indicates_authentication_failure,
        validate_http_response,
    };

    #[test]
    fn validates_success_statuses() {
        assert!(validate_http_response(204, &[], None).is_ok());
    }

    #[test]
    fn maps_auth_statuses_like_swift_client() {
        assert!(matches!(
            validate_http_response(401, &[], None),
            Err(GrokError::Unauthorized)
        ));
        assert!(matches!(
            validate_http_response(403, br#"{"error":"cookie expired"}"#, None),
            Err(GrokError::Unauthorized)
        ));
    }

    #[test]
    fn maps_forbidden_to_actionable_access_denied_message() {
        match validate_http_response(403, br#"{"error":"forbidden"}"#, Some("expert")) {
            Ok(()) => panic!("expected access denied"),
            Err(GrokError::AccessDenied(message)) => {
                assert!(message.contains("Grok denied access to Expert (expert)."));
                assert!(message.contains("--model fast"));
            }
            Err(other) => panic!("expected access denied, got {other:?}"),
        }
    }

    #[test]
    fn includes_capped_non_2xx_body_in_api_error() {
        let error = validate_http_response(429, br#"{"error":{"message":"rate limited"}}"#, None);

        assert!(
            matches!(error, Err(GrokError::Api(message)) if message.contains("HTTP Error: 429") && message.contains("rate limited"))
        );
    }

    #[test]
    fn extracts_error_body_messages() {
        assert_eq!(
            http_error_body_message(br#"{"error":{"message":"bad auth"}}"#).as_deref(),
            Some("bad auth")
        );
        assert!(response_body_indicates_authentication_failure(Some(
            "not authenticated"
        )));
    }
}
