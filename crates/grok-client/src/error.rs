use thiserror::Error;

pub type Result<T> = std::result::Result<T, GrokError>;

#[derive(Debug, Error)]
pub enum GrokError {
    #[error("Invalid or missing Grok credentials")]
    InvalidCredentials,
    #[error("Grok rejected the saved cookies. Re-run `grok auth generate` after logging in.")]
    Unauthorized,
    #[error("{0}")]
    AccessDenied(String),
    #[error("Grok API endpoint was not found")]
    NotFound(String),
    #[error("Network error: {0}")]
    Network(String),
    #[error("Could not decode Grok response: {0}")]
    Decoding(String),
    #[error("{0}")]
    Api(String),
    #[error("Could not read Grok streaming response")]
    Streaming(String),
    #[error("URL error: {0}")]
    Url(String),
}

impl From<serde_json::Error> for GrokError {
    fn from(error: serde_json::Error) -> Self {
        Self::Decoding(error.to_string())
    }
}

impl From<reqwest::Error> for GrokError {
    fn from(error: reqwest::Error) -> Self {
        Self::Network(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::GrokError;

    #[test]
    fn display_strings_match_swift_localized_error_contract() {
        assert_eq!(
            GrokError::InvalidCredentials.to_string(),
            "Invalid or missing Grok credentials"
        );
        assert_eq!(
            GrokError::Unauthorized.to_string(),
            "Grok rejected the saved cookies. Re-run `grok auth generate` after logging in."
        );
        assert_eq!(
            GrokError::NotFound("ignored".to_string()).to_string(),
            "Grok API endpoint was not found"
        );
        assert_eq!(
            GrokError::Decoding("bad json".to_string()).to_string(),
            "Could not decode Grok response: bad json"
        );
        assert_eq!(
            GrokError::Api("HTTP Error: 429 rate limited".to_string()).to_string(),
            "HTTP Error: 429 rate limited"
        );
        assert_eq!(
            GrokError::Streaming("partial line".to_string()).to_string(),
            "Could not read Grok streaming response"
        );
    }
}
