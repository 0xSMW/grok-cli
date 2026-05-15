use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::JsonLookup;

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokMode {
    pub id: String,
    pub display_name: String,
    pub summary: String,
    pub is_available: bool,
    pub unavailable_reason: Option<String>,
    pub minimum_subscription_tier: Option<String>,
}

impl GrokMode {
    pub fn new(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            display_name: id.clone(),
            id,
            summary: String::new(),
            is_available: true,
            unavailable_reason: None,
            minimum_subscription_tier: None,
        }
    }

    pub fn with_summary(
        id: impl Into<String>,
        display_name: impl Into<String>,
        summary: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            summary: summary.into(),
            is_available: true,
            unavailable_reason: None,
            minimum_subscription_tier: None,
        }
    }

    pub fn unavailable_description(&self) -> Option<String> {
        if self.is_available {
            return None;
        }

        if let Some(reason) = self
            .unavailable_reason
            .as_ref()
            .filter(|value| !value.is_empty())
        {
            return Some(reason.clone());
        }

        if let Some(tier) = self
            .minimum_subscription_tier
            .as_ref()
            .filter(|value| !value.is_empty())
        {
            return Some(format!("Requires {tier}"));
        }

        Some("Unavailable for this account".to_string())
    }

    pub fn auto() -> Self {
        Self::with_summary("auto", "Auto", "Chooses Fast or Expert")
    }

    pub fn fast() -> Self {
        Self::with_summary("fast", "Fast", "Quick responses")
    }

    pub fn expert() -> Self {
        Self::with_summary("expert", "Expert", "Thinks hard")
    }

    pub fn grok43_beta() -> Self {
        Self::with_summary(
            "grok-420-computer-use-sa",
            "Grok 4.3 (beta)",
            "Uses Skills and Connectors",
        )
    }

    pub fn heavy() -> Self {
        Self::with_summary("heavy", "Heavy", "Team of Experts")
    }

    pub fn default_mode() -> Self {
        Self::fast()
    }

    pub fn known_modes() -> Vec<Self> {
        vec![
            Self::auto(),
            Self::fast(),
            Self::expert(),
            Self::grok43_beta(),
            Self::heavy(),
        ]
    }

    pub fn resolve(raw_value: Option<&str>) -> Self {
        let Some(trimmed) = raw_value.map(str::trim).filter(|value| !value.is_empty()) else {
            return Self::default_mode();
        };

        match normalized_token(trimmed).as_str() {
            "auto" => Self::auto(),
            "fast" => Self::fast(),
            "expert" | "reasoning" | "think" => Self::expert(),
            "heavy" => Self::heavy(),
            "grok-4.3"
            | "grok-4-3"
            | "grok-43"
            | "4.3"
            | "43"
            | "beta"
            | "grok-4.3-beta"
            | "grok-4-3-beta"
            | "grok-43-beta"
            | "grok-420"
            | "grok-420-computer-use-sa" => Self::grok43_beta(),
            _ => {
                let mut mode = Self::new(trimmed);
                mode.summary = "Custom web mode ID".to_string();
                mode
            }
        }
    }

    pub fn resolve_from_modes(raw_value: Option<&str>, modes: &[Self]) -> Self {
        let Some(trimmed) = raw_value.map(str::trim).filter(|value| !value.is_empty()) else {
            return modes
                .iter()
                .find(|mode| mode.id == Self::default_mode().id)
                .cloned()
                .unwrap_or_else(Self::default_mode);
        };

        let normalized = normalized_token(trimmed);
        if let Some(mode) = modes.iter().find(|mode| {
            normalized_token(&mode.id) == normalized
                || normalized_token(&mode.display_name) == normalized
        }) {
            return mode.clone();
        }

        let resolved = Self::resolve(Some(trimmed));
        modes
            .iter()
            .find(|mode| mode.id == resolved.id)
            .cloned()
            .unwrap_or(resolved)
    }

    pub fn from_dictionary(dictionary: Map<String, Value>) -> Option<Self> {
        let lookup = JsonLookup::new(Value::Object(dictionary.clone()));
        let id = lookup.string(&[
            "id", "modeId", "mode_id", "modelId", "model_id", "slug", "value",
        ])?;
        let availability = mode_availability(&dictionary);

        Some(Self {
            id: id.clone(),
            display_name: lookup
                .string(&["displayName", "display_name", "name", "title", "label"])
                .unwrap_or(id),
            summary: lookup
                .string(&["summary", "description", "subtitle"])
                .unwrap_or_default(),
            is_available: availability.is_available,
            unavailable_reason: availability.reason,
            minimum_subscription_tier: availability.minimum_subscription_tier,
        })
    }

    pub fn modes_from_raw_json(raw_json: &Value) -> Vec<Self> {
        mode_dictionaries(raw_json)
            .into_iter()
            .filter_map(Self::from_dictionary)
            .fold(Vec::new(), |mut modes, mode| {
                if !modes.iter().any(|existing: &Self| existing.id == mode.id) {
                    modes.push(mode);
                }
                modes
            })
    }
}

fn normalized_token(value: &str) -> String {
    value.to_lowercase().replace(['_', ' '], "-")
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ParsedModeAvailability {
    is_available: bool,
    reason: Option<String>,
    minimum_subscription_tier: Option<String>,
}

fn mode_dictionaries(raw_json: &Value) -> Vec<Map<String, Value>> {
    JsonLookup::new(raw_json.clone()).dictionaries(&[
        "modes",
        "modeItems",
        "mode_items",
        "models",
        "data",
        "result",
        "items",
    ])
}

fn mode_availability(dictionary: &Map<String, Value>) -> ParsedModeAvailability {
    let lookup = JsonLookup::new(Value::Object(dictionary.clone()));

    if let Some(availability) = dictionary.get("availability").and_then(Value::as_object) {
        let availability_lookup = JsonLookup::new(Value::Object(availability.clone()));
        if let Some(available_value) = availability.get("available") {
            if let Some(available) = available_value.as_bool() {
                return ParsedModeAvailability {
                    is_available: available,
                    reason: (!available)
                        .then(|| availability_lookup.string(&["message", "reason", "description"]))
                        .flatten(),
                    minimum_subscription_tier: availability_lookup
                        .string(&["minimumSubscriptionTier", "minimum_subscription_tier"]),
                };
            }
            return ParsedModeAvailability::available();
        }

        if let Some(requires_upgrade) = availability
            .get("requiresUpgrade")
            .or_else(|| availability.get("requires_upgrade"))
        {
            if let Some(dictionary) = requires_upgrade.as_object() {
                let lookup = JsonLookup::new(Value::Object(dictionary.clone()));
                return ParsedModeAvailability {
                    is_available: false,
                    reason: lookup.string(&["message", "reason", "description"]),
                    minimum_subscription_tier: lookup
                        .string(&["minimumSubscriptionTier", "minimum_subscription_tier"]),
                };
            }
            if requires_upgrade.as_bool() == Some(true) {
                return ParsedModeAvailability {
                    is_available: false,
                    reason: availability_lookup.string(&["message", "reason", "description"]),
                    minimum_subscription_tier: availability_lookup
                        .string(&["minimumSubscriptionTier", "minimum_subscription_tier"]),
                };
            }
        }

        if let Some(unavailable) = availability
            .get("unavailable")
            .or_else(|| availability.get("disabled"))
            && let Some(dictionary) = unavailable.as_object()
        {
            let lookup = JsonLookup::new(Value::Object(dictionary.clone()));
            return ParsedModeAvailability {
                is_available: false,
                reason: lookup.string(&["message", "reason", "description"]),
                minimum_subscription_tier: lookup
                    .string(&["minimumSubscriptionTier", "minimum_subscription_tier"]),
            };
        }
    }

    if let Some(is_available) =
        direct_bool(dictionary, &["available", "isAvailable", "is_available"])
    {
        return ParsedModeAvailability {
            is_available,
            reason: (!is_available)
                .then(|| {
                    lookup.string(&[
                        "unavailableReason",
                        "unavailable_reason",
                        "reason",
                        "message",
                    ])
                })
                .flatten(),
            minimum_subscription_tier: lookup
                .string(&["minimumSubscriptionTier", "minimum_subscription_tier"]),
        };
    }

    if direct_bool(dictionary, &["disabled", "isDisabled", "is_disabled"]) == Some(true) {
        return ParsedModeAvailability {
            is_available: false,
            reason: lookup.string(&[
                "unavailableReason",
                "unavailable_reason",
                "reason",
                "message",
            ]),
            minimum_subscription_tier: lookup
                .string(&["minimumSubscriptionTier", "minimum_subscription_tier"]),
        };
    }

    ParsedModeAvailability::available()
}

impl ParsedModeAvailability {
    fn available() -> Self {
        Self {
            is_available: true,
            reason: None,
            minimum_subscription_tier: None,
        }
    }
}

fn direct_bool(dictionary: &Map<String, Value>, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| dictionary.get(*key).and_then(Value::as_bool))
}

#[cfg(test)]
mod tests {
    use super::GrokMode;

    #[test]
    fn resolves_swift_mode_aliases() {
        assert_eq!(GrokMode::resolve(None).id, "fast");
        assert_eq!(GrokMode::resolve(Some("expert")).id, "expert");
        assert_eq!(GrokMode::resolve(Some("think")).id, "expert");
        assert_eq!(
            GrokMode::resolve(Some("grok-4.3-beta")).id,
            "grok-420-computer-use-sa"
        );
        assert_eq!(GrokMode::resolve(Some("new-web-mode")).id, "new-web-mode");
    }

    #[test]
    fn resolves_against_dynamic_mode_catalog() {
        let modes = vec![
            GrokMode::with_summary("custom-id", "Mode Name", "catalog"),
            GrokMode::fast(),
        ];

        assert_eq!(
            GrokMode::resolve_from_modes(Some("mode name"), &modes).id,
            "custom-id"
        );
        assert_eq!(GrokMode::resolve_from_modes(None, &modes).id, "fast");
    }

    #[test]
    fn parses_dynamic_modes_with_availability_like_swift() {
        let modes = GrokMode::modes_from_raw_json(&serde_json::json!({
            "modes": [
                {
                    "id": "fast",
                    "displayName": "Fast",
                    "summary": "Quick responses",
                    "availability": { "available": {} }
                },
                {
                    "modeId": "heavy",
                    "name": "Heavy",
                    "description": "Team of Experts",
                    "availability": {
                        "requiresUpgrade": {
                            "message": "",
                            "minimumSubscriptionTier": "TIER_SUPERGROK_HEAVY"
                        }
                    }
                }
            ]
        }));

        assert_eq!(modes.len(), 2);
        assert_eq!(modes[0].id, "fast");
        assert!(modes[0].is_available);
        assert_eq!(modes[1].id, "heavy");
        assert_eq!(modes[1].display_name, "Heavy");
        assert!(!modes[1].is_available);
        assert_eq!(
            modes[1].minimum_subscription_tier.as_deref(),
            Some("TIER_SUPERGROK_HEAVY")
        );
        assert_eq!(
            modes[1].unavailable_description().as_deref(),
            Some("Requires TIER_SUPERGROK_HEAVY")
        );
    }

    #[test]
    fn mode_availability_only_accepts_boolean_flags_like_swift() {
        let modes = GrokMode::modes_from_raw_json(&serde_json::json!({
            "modes": [
                {
                    "id": "nested-string-available",
                    "availability": {
                        "available": "false",
                        "message": "string false should be ignored"
                    }
                },
                {
                    "id": "string-upgrade",
                    "availability": {
                        "requiresUpgrade": "true",
                        "message": "string true should be ignored"
                    }
                },
                {
                    "id": "top-level-string-available",
                    "available": "false",
                    "unavailableReason": "string false should be ignored"
                },
                {
                    "id": "top-level-string-disabled",
                    "disabled": "true",
                    "unavailableReason": "string true should be ignored"
                },
                {
                    "id": "boolean-unavailable",
                    "available": false,
                    "unavailableReason": "real boolean false"
                }
            ]
        }));

        assert_eq!(modes.len(), 5);
        assert!(modes[0].is_available);
        assert!(modes[1].is_available);
        assert!(modes[2].is_available);
        assert!(modes[3].is_available);
        assert!(!modes[4].is_available);
        assert_eq!(
            modes[4].unavailable_description().as_deref(),
            Some("real boolean false")
        );
    }
}
