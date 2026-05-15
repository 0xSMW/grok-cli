use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use grok_client::required_auth_cookie_names;

pub fn config_directory() -> PathBuf {
    if let Ok(path) = std::env::var("GROK_CONFIG_DIR")
        && !path.trim().is_empty()
    {
        return expand_tilde(path);
    }

    if cfg!(target_os = "macos") {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".config/grok-cli")
    } else {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".grok-cli")
    }
}

pub fn credentials_path() -> PathBuf {
    config_directory().join("credentials.json")
}

pub fn saved_credentials_path() -> Option<PathBuf> {
    let path = credentials_path();
    path.exists().then_some(path)
}

pub fn load_credentials() -> Result<BTreeMap<String, String>> {
    let path = credentials_path();
    let data = fs::read(&path)
        .with_context(|| format!("Could not read credentials file {}", path.display()))?;
    validate_credential_cookies(&data, "Credentials file")
}

pub fn validate_credential_cookies(
    bytes: &[u8],
    context: &str,
) -> Result<BTreeMap<String, String>> {
    let cookies: BTreeMap<String, String> = serde_json::from_slice(bytes).with_context(|| {
        format!("{context} must be a JSON object whose keys and values are strings")
    })?;

    if cookies.is_empty() {
        bail!("{context} did not contain any cookies");
    }

    let empty_keys = cookies
        .iter()
        .filter_map(|(key, value)| value.trim().is_empty().then_some(key.as_str()))
        .collect::<Vec<_>>();
    if !empty_keys.is_empty() {
        bail!(
            "{} contains empty cookie values for: {}",
            context,
            empty_keys.join(", ")
        );
    }

    let auth_cookie_names = required_auth_cookie_names();
    if !cookies
        .keys()
        .any(|key| auth_cookie_names.contains(key.to_lowercase().as_str()))
    {
        bail!(
            "{context} must include at least one Grok auth cookie: sso, sso-rw, x-userid, or x-anonuserid"
        );
    }

    Ok(cookies)
}

pub fn save_credentials_path(source_path: impl Into<PathBuf>) -> Result<PathBuf> {
    let source_path = expand_tilde_path(source_path.into());
    let data = fs::read(&source_path)
        .with_context(|| format!("Could not read credentials file {}", source_path.display()))?;
    validate_credential_cookies(&data, "Credentials file")?;

    let target_path = credentials_path();
    let parent = target_path
        .parent()
        .ok_or_else(|| anyhow!("Credentials path has no parent"))?;
    fs::create_dir_all(parent)?;
    fs::write(&target_path, data)?;
    set_user_read_write(&target_path)?;
    Ok(target_path)
}

pub fn run_cookie_extractor(extra_args: &[String], suppress_output: bool) -> Result<PathBuf> {
    let target_path = credentials_path();
    let parent = target_path
        .parent()
        .ok_or_else(|| anyhow!("Credentials path has no parent"))?;
    fs::create_dir_all(parent)?;

    let extractor_path = find_cookie_extractor().ok_or_else(|| {
        anyhow!("Could not find cookie_extractor.py. Run from the grok-cli checkout or reinstall the CLI.")
    })?;

    let mut command = Command::new("/usr/bin/env");
    command.arg("python3").arg(&extractor_path);
    command.args(extra_args);
    command.args(["--format", "json", "--required", "--output"]);
    command.arg(&target_path);
    if suppress_output {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }

    let status = command.status().with_context(|| {
        format!(
            "Could not run cookie extractor {}",
            extractor_path.display()
        )
    })?;
    if !status.success() {
        bail!(
            "Cookie extraction failed with exit code {}",
            status.code().unwrap_or(-1)
        );
    }

    let data = fs::read(&target_path).with_context(|| {
        format!(
            "Could not read cookie extractor output {}",
            target_path.display()
        )
    })?;
    validate_credential_cookies(&data, "Cookie extractor output")?;
    set_user_read_write(&target_path)?;
    Ok(target_path)
}

fn find_cookie_extractor() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("GROK_COOKIE_EXTRACTOR") {
        let path = expand_tilde(path);
        if path.exists() {
            return Some(path);
        }
    }

    let current_dir = std::env::current_dir().ok();
    let executable_dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(PathBuf::from));

    let mut candidates = Vec::new();
    if let Some(current_dir) = current_dir {
        candidates.push(current_dir.join("Scripts/cookie_extractor.py"));
        candidates.push(current_dir.join("cookie_extractor.py"));
    }
    if let Some(executable_dir) = executable_dir {
        candidates.push(executable_dir.join("cookie_extractor.py"));
        candidates.push(executable_dir.join("../Scripts/cookie_extractor.py"));
    }

    candidates.into_iter().find(|path| path.exists())
}

fn expand_tilde(path: String) -> PathBuf {
    expand_tilde_path(PathBuf::from(path))
}

fn expand_tilde_path(path: PathBuf) -> PathBuf {
    let Some(value) = path.to_str() else {
        return path;
    };
    if value == "~" {
        return dirs::home_dir().unwrap_or(path);
    }
    if let Some(rest) = value.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    path
}

#[cfg(unix)]
fn set_user_read_write(path: &PathBuf) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_user_read_write(_path: &PathBuf) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_credential_cookies;

    #[test]
    fn validates_cookie_json() -> anyhow::Result<()> {
        let cookies = validate_credential_cookies(
            br#"{"sso":"cookie","x-anonuserid":"anon"}"#,
            "Credentials file",
        )?;

        assert_eq!(cookies.get("sso").map(String::as_str), Some("cookie"));
        Ok(())
    }

    #[test]
    fn rejects_missing_auth_cookie() {
        let Err(error) = validate_credential_cookies(br#"{"other":"cookie"}"#, "Credentials file")
        else {
            panic!("auth cookie should be required");
        };

        assert!(
            error
                .to_string()
                .contains("must include at least one Grok auth cookie")
        );
    }

    #[test]
    fn rejects_empty_cookie_values() {
        let Err(error) = validate_credential_cookies(br#"{"sso":"   "}"#, "Credentials file")
        else {
            panic!("empty cookie value should be rejected");
        };

        assert!(
            error
                .to_string()
                .contains("contains empty cookie values for: sso")
        );
    }
}
