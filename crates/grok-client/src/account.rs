use chrono::{DateTime, FixedOffset};
use serde_json::{Map, Value};
use std::time::{Duration, SystemTime};

use crate::{GrokClient, GrokMode, JsonLookup, Result};

#[derive(Clone, Debug, PartialEq)]
pub struct GrokTypeaheadSuggestion {
    pub text: String,
    pub title: Option<String>,
    pub raw_json: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokTypeaheadResponse {
    pub suggestions: Vec<GrokTypeaheadSuggestion>,
    pub raw_json: Value,
}

impl GrokTypeaheadResponse {
    pub fn from_raw_json(raw_json: Value, max_items: usize) -> Self {
        let mut seen = Vec::<String>::new();
        let suggestions = typeahead_values(&raw_json)
            .into_iter()
            .filter_map(typeahead_suggestion)
            .filter(|suggestion| {
                let normalized = suggestion.text.to_lowercase();
                if seen.iter().any(|value| value == &normalized) {
                    return false;
                }
                seen.push(normalized);
                true
            })
            .take(max_items)
            .collect();

        Self {
            suggestions,
            raw_json,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokModesResponse {
    pub modes: Vec<GrokMode>,
    pub raw_json: Value,
}

impl GrokModesResponse {
    pub fn from_raw_json(raw_json: Value) -> Self {
        Self {
            modes: GrokMode::modes_from_raw_json(&raw_json),
            raw_json,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokSubscription {
    pub tier: Option<String>,
    pub name: Option<String>,
    pub status: Option<String>,
    pub is_active: bool,
    pub raw_json: Value,
}

impl GrokSubscription {
    pub fn display_name(&self) -> String {
        subscription_display_name(
            self.tier.as_deref(),
            self.name.as_deref(),
            self.raw_json.as_object(),
        )
    }

    fn from_dictionary(dictionary: Map<String, Value>) -> Self {
        let lookup = JsonLookup::new(Value::Object(dictionary.clone()));
        Self {
            tier: lookup.first_string(&[
                "subscriptionTier",
                "subscription_tier",
                "tier",
                "tierName",
                "tier_name",
                "planTier",
                "plan_tier",
                "sku",
            ]),
            name: lookup.first_string(&[
                "displayName",
                "display_name",
                "name",
                "title",
                "planName",
                "plan_name",
                "productName",
                "product_name",
            ]),
            status: lookup.first_string(&[
                "status",
                "subscriptionStatus",
                "subscription_status",
                "state",
            ]),
            is_active: is_active_subscription(&Value::Object(dictionary.clone())),
            raw_json: Value::Object(dictionary),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokSubscriptionsResponse {
    pub subscriptions: Vec<GrokSubscription>,
    pub current_subscription: Option<GrokSubscription>,
    pub raw_json: Value,
}

impl GrokSubscriptionsResponse {
    pub fn from_raw_json(raw_json: Value) -> Self {
        let subscriptions = subscription_dictionaries(&raw_json)
            .into_iter()
            .map(GrokSubscription::from_dictionary)
            .collect::<Vec<_>>();
        let current_subscription = subscriptions
            .iter()
            .find(|subscription| subscription.is_active)
            .cloned();

        Self {
            subscriptions,
            current_subscription,
            raw_json,
        }
    }

    pub fn display_name(&self) -> String {
        self.current_subscription
            .as_ref()
            .map(GrokSubscription::display_name)
            .unwrap_or_else(|| "Grok".to_string())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct GrokRateLimit {
    pub model_name: Option<String>,
    pub remaining_responses: Option<i64>,
    pub reset_at: Option<SystemTime>,
    pub reset_after_seconds: Option<i64>,
    pub window_seconds: Option<i64>,
    pub fetched_at: SystemTime,
    pub raw_json: Value,
}

impl GrokRateLimit {
    pub fn from_raw_json(raw_json: Value, requested_model_name: &str) -> Self {
        Self::from_raw_json_at(raw_json, requested_model_name, SystemTime::now())
    }

    pub fn from_raw_json_at(
        raw_json: Value,
        requested_model_name: &str,
        fetched_at: SystemTime,
    ) -> Self {
        let rate_limit = matching_rate_limit_dictionary(&raw_json, requested_model_name)
            .or_else(|| first_rate_limit_dictionary(&raw_json))
            .unwrap_or_default();
        let lookup = JsonLookup::new(Value::Object(rate_limit.clone()));
        let model_name = lookup
            .first_string(&["modelName", "model", "modelId", "modeId", "name"])
            .or_else(|| {
                (!requested_model_name.trim().is_empty())
                    .then(|| requested_model_name.trim().to_string())
            });

        Self {
            model_name,
            remaining_responses: first_int(
                &rate_limit,
                &[
                    "remainingResponses",
                    "remainingResponseCount",
                    "responsesRemaining",
                    "remainingMessages",
                    "messagesRemaining",
                    "remainingRequests",
                    "requestsRemaining",
                    "remainingQueries",
                    "queriesRemaining",
                    "remaining",
                ],
            ),
            reset_at: first_date_time(
                &rate_limit,
                &[
                    "resetAt",
                    "resetsAt",
                    "resetTime",
                    "resetTimestamp",
                    "windowResetAt",
                    "rateLimitResetAt",
                    "expiresAt",
                    "reset",
                ],
                fetched_at,
            ),
            reset_after_seconds: first_duration_seconds(
                &rate_limit,
                &[
                    "resetAfterSeconds",
                    "secondsUntilReset",
                    "resetInSeconds",
                    "timeUntilResetSeconds",
                    "resetAfter",
                    "resetIn",
                    "ttlSeconds",
                ],
            ),
            window_seconds: first_duration_seconds(
                &rate_limit,
                &[
                    "windowSeconds",
                    "windowLengthSeconds",
                    "windowDurationSeconds",
                    "limitWindowSeconds",
                    "rateLimitWindowSeconds",
                    "periodSeconds",
                    "durationSeconds",
                    "window",
                    "windowLength",
                    "windowDuration",
                    "limitWindow",
                    "rateLimitWindow",
                    "period",
                    "duration",
                ],
            ),
            fetched_at,
            raw_json,
        }
    }

    pub fn is_low(&self) -> bool {
        self.remaining_responses
            .is_some_and(|remaining| remaining < 10)
    }

    pub fn seconds_until_reset(&self, now: SystemTime) -> Option<i64> {
        if let Some(reset_at) = self.reset_at {
            return Some(
                reset_at
                    .duration_since(now)
                    .ok()
                    .map(ceil_duration_seconds)
                    .unwrap_or(0),
            );
        }

        let reset_after_seconds = self.reset_after_seconds?;
        let elapsed = now
            .duration_since(self.fetched_at)
            .ok()
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or(0);
        Some((reset_after_seconds - elapsed).max(0))
    }
}

fn ceil_duration_seconds(duration: Duration) -> i64 {
    duration.as_secs() as i64 + i64::from(duration.subsec_nanos() > 0)
}

impl GrokClient {
    pub async fn typeahead_response(
        &self,
        query: &str,
        lang: &str,
        max_items: usize,
        platform: &str,
        source: usize,
    ) -> Result<GrokTypeaheadResponse> {
        let Some(request) = self.typeahead_request(query, lang, max_items, platform, source)?
        else {
            return Ok(GrokTypeaheadResponse {
                suggestions: Vec::new(),
                raw_json: Value::Object(Default::default()),
            });
        };
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokTypeaheadResponse::from_raw_json(raw_json, max_items))
    }

    pub async fn list_modes_response(&self) -> Result<GrokModesResponse> {
        let request = self.list_modes_request()?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokModesResponse::from_raw_json(raw_json))
    }

    pub async fn list_modes(&self) -> Result<Vec<GrokMode>> {
        Ok(self.list_modes_response().await?.modes)
    }

    pub async fn subscriptions_response(&self) -> Result<GrokSubscriptionsResponse> {
        let request = self.subscriptions_request()?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokSubscriptionsResponse::from_raw_json(raw_json))
    }

    pub async fn current_subscription(&self) -> Result<Option<GrokSubscription>> {
        Ok(self.subscriptions_response().await?.current_subscription)
    }

    pub async fn rate_limits(&self, model_name: &str) -> Result<GrokRateLimit> {
        let resolved_model_name = if model_name.trim().is_empty() {
            GrokMode::default_mode().id
        } else {
            model_name.trim().to_string()
        };
        let request = self.rate_limits_request(&resolved_model_name)?;
        let raw_json = self.send_request_json(request).await?;
        Ok(GrokRateLimit::from_raw_json(raw_json, &resolved_model_name))
    }

    pub async fn rate_limits_for_mode(&self, mode: &GrokMode) -> Result<GrokRateLimit> {
        self.rate_limits(&mode.id).await
    }
}

fn typeahead_values(value: &Value) -> Vec<Value> {
    if let Some(array) = value.as_array() {
        return array.clone();
    }

    let Some(dictionary) = value.as_object() else {
        return Vec::new();
    };

    if typeahead_text(dictionary).is_some() {
        return vec![value.clone()];
    }

    for key in [
        "suggestions",
        "items",
        "results",
        "data",
        "result",
        "queries",
        "completions",
    ] {
        if let Some(nested) = dictionary.get(key) {
            let values = typeahead_values(nested);
            if !values.is_empty() {
                return values;
            }
        }
    }

    Vec::new()
}

fn typeahead_suggestion(value: Value) -> Option<GrokTypeaheadSuggestion> {
    match value {
        Value::String(text) => {
            let text = normalized_typeahead_text(&text);
            (!text.is_empty()).then_some(GrokTypeaheadSuggestion {
                text,
                title: None,
                raw_json: Value::Null,
            })
        }
        Value::Object(dictionary) => {
            let text = typeahead_text(&dictionary)?;
            let title = JsonLookup::new(Value::Object(dictionary.clone()))
                .string(&["title", "label", "display", "name"])
                .filter(|title| title != &text);
            Some(GrokTypeaheadSuggestion {
                text,
                title,
                raw_json: Value::Object(dictionary),
            })
        }
        _ => None,
    }
}

fn typeahead_text(dictionary: &Map<String, Value>) -> Option<String> {
    [
        "text",
        "query",
        "value",
        "completion",
        "suggestion",
        "title",
        "name",
        "label",
    ]
    .iter()
    .find_map(|key| dictionary.get(*key).and_then(Value::as_str))
    .map(normalized_typeahead_text)
    .filter(|text| !text.is_empty())
}

fn normalized_typeahead_text(value: &str) -> String {
    value.replace(['\r', '\n'], " ").trim().to_string()
}

fn subscription_dictionaries(value: &Value) -> Vec<Map<String, Value>> {
    if let Some(array) = value.as_array() {
        return array
            .iter()
            .flat_map(|nested| {
                nested
                    .as_object()
                    .cloned()
                    .map(|dictionary| vec![dictionary])
                    .unwrap_or_else(|| subscription_dictionaries(nested))
            })
            .collect();
    }

    let Some(dictionary) = value.as_object() else {
        return Vec::new();
    };

    for key in [
        "currentSubscription",
        "current_subscription",
        "activeSubscription",
        "active_subscription",
        "subscription",
        "subscriptions",
        "activeSubscriptions",
        "active_subscriptions",
        "userSubscriptions",
        "user_subscriptions",
        "accountSubscriptions",
        "account_subscriptions",
        "data",
        "result",
        "items",
    ] {
        if let Some(nested) = dictionary.get(key) {
            let found = subscription_dictionaries(nested);
            if !found.is_empty() {
                return found;
            }
        }
    }

    if looks_like_subscription(dictionary) {
        return vec![dictionary.clone()];
    }

    dictionary
        .values()
        .find_map(|nested| {
            let found = subscription_dictionaries(nested);
            (!found.is_empty()).then_some(found)
        })
        .unwrap_or_default()
}

fn looks_like_subscription(dictionary: &Map<String, Value>) -> bool {
    contains_any_key(
        dictionary,
        &[
            "subscriptionTier",
            "subscription_tier",
            "tier",
            "tierName",
            "tier_name",
            "planName",
            "plan_name",
            "productName",
            "product_name",
            "subscriptionStatus",
            "subscription_status",
            "currentPeriodEnd",
            "current_period_end",
        ],
    )
}

fn is_active_subscription(value: &Value) -> bool {
    if let Some(active) = first_bool(
        value,
        &[
            "isActive",
            "is_active",
            "active",
            "current",
            "isCurrent",
            "is_current",
            "subscribed",
            "isSubscribed",
            "is_subscribed",
            "hasActiveSubscription",
            "has_active_subscription",
        ],
    ) {
        return active;
    }

    if let Some(status) = JsonLookup::new(value.clone()).first_string(&[
        "status",
        "subscriptionStatus",
        "subscription_status",
        "state",
    ]) {
        let normalized = status.trim().to_lowercase().replace(' ', "_");
        if ["active", "trialing", "current", "subscribed", "paid"].contains(&normalized.as_str()) {
            return true;
        }
        if [
            "canceled",
            "cancelled",
            "expired",
            "inactive",
            "pastdue",
            "past_due",
            "unpaid",
        ]
        .contains(&normalized.as_str())
        {
            return false;
        }
    }

    true
}

fn first_bool(value: &Value, keys: &[&str]) -> Option<bool> {
    match value {
        Value::Object(dictionary) => {
            for key in keys {
                if let Some(bool) = dictionary.get(*key).and_then(bool_value) {
                    return Some(bool);
                }
            }
            dictionary
                .values()
                .find_map(|nested| first_bool(nested, keys))
        }
        Value::Array(values) => values.iter().find_map(|nested| first_bool(nested, keys)),
        _ => None,
    }
}

fn bool_value(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        Value::Number(value) => value.as_i64().map(|number| number != 0),
        Value::String(value) => match value.trim().to_lowercase().as_str() {
            "true" | "yes" | "1" | "active" | "current" | "subscribed" => Some(true),
            "false" | "no" | "0" | "inactive" | "expired" | "canceled" | "cancelled" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn subscription_display_name(
    tier: Option<&str>,
    name: Option<&str>,
    raw_json: Option<&Map<String, Value>>,
) -> String {
    let mut tokens = Vec::new();
    tokens.extend(tier);
    tokens.extend(name);
    if let Some(raw_json) = raw_json {
        for key in [
            "subscriptionTier",
            "subscription_tier",
            "tier",
            "tierName",
            "tier_name",
            "plan",
            "planName",
            "plan_name",
            "productName",
            "product_name",
            "displayName",
            "display_name",
            "name",
            "title",
            "sku",
        ] {
            if let Some(value) = raw_json.get(key).and_then(Value::as_str) {
                tokens.push(value);
            }
        }
    }

    let normalized = tokens
        .into_iter()
        .map(normalized_plan_token)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if normalized.iter().any(|value| value.contains("heavy")) {
        return "SuperGrok Heavy".to_string();
    }
    if normalized.iter().any(|value| {
        value.contains("supergrok") || (value.contains("super") && value.contains("grok"))
    }) {
        return "SuperGrok".to_string();
    }
    "Grok".to_string()
}

fn normalized_plan_token(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn matching_rate_limit_dictionary(
    value: &Value,
    requested_model_name: &str,
) -> Option<Map<String, Value>> {
    if requested_model_name.trim().is_empty() {
        return None;
    }
    match value {
        Value::Object(dictionary) => {
            if let Some(nested) = dictionary.get(requested_model_name)
                && let Some(found) = first_rate_limit_dictionary(nested)
            {
                return Some(found);
            }
            if matches_rate_limit_model(dictionary, requested_model_name) {
                return Some(dictionary.clone());
            }
            dictionary
                .values()
                .find_map(|nested| matching_rate_limit_dictionary(nested, requested_model_name))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|nested| matching_rate_limit_dictionary(nested, requested_model_name)),
        _ => None,
    }
}

fn first_rate_limit_dictionary(value: &Value) -> Option<Map<String, Value>> {
    match value {
        Value::Object(dictionary) => {
            if looks_like_rate_limit(dictionary) {
                return Some(dictionary.clone());
            }
            for key in [
                "rateLimit",
                "rateLimits",
                "limits",
                "usage",
                "data",
                "result",
                "models",
                "items",
                "values",
            ] {
                if let Some(nested) = dictionary.get(key)
                    && let Some(found) = first_rate_limit_dictionary(nested)
                {
                    return Some(found);
                }
            }
            dictionary.values().find_map(first_rate_limit_dictionary)
        }
        Value::Array(values) => values.iter().find_map(first_rate_limit_dictionary),
        _ => None,
    }
}

fn looks_like_rate_limit(dictionary: &Map<String, Value>) -> bool {
    contains_any_key(
        dictionary,
        &[
            "remainingResponses",
            "remainingResponseCount",
            "responsesRemaining",
            "remainingMessages",
            "messagesRemaining",
            "remainingRequests",
            "requestsRemaining",
            "remainingQueries",
            "queriesRemaining",
            "remaining",
            "resetAfterSeconds",
            "secondsUntilReset",
            "resetInSeconds",
            "timeUntilResetSeconds",
            "resetAfter",
            "resetIn",
            "ttlSeconds",
            "windowSeconds",
            "window",
        ],
    )
}

fn matches_rate_limit_model(dictionary: &Map<String, Value>, requested_model_name: &str) -> bool {
    direct_string(
        dictionary,
        &["modelName", "model", "modelId", "modeId", "name"],
    )
    .is_some_and(|model_name| model_name == requested_model_name)
}

fn direct_string(dictionary: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    JsonLookup::new(Value::Object(dictionary.clone())).string(keys)
}

fn first_int(dictionary: &Map<String, Value>, keys: &[&str]) -> Option<i64> {
    JsonLookup::new(Value::Object(dictionary.clone())).first_int(keys)
}

fn first_duration_seconds(dictionary: &Map<String, Value>, keys: &[&str]) -> Option<i64> {
    keys.iter()
        .find_map(|key| dictionary.get(*key).and_then(duration_seconds))
}

fn first_date_time(
    dictionary: &Map<String, Value>,
    keys: &[&str],
    fetched_at: SystemTime,
) -> Option<SystemTime> {
    keys.iter().find_map(|key| {
        dictionary
            .get(*key)
            .and_then(|value| date_time(value, fetched_at))
    })
}

fn date_time(value: &Value, fetched_at: SystemTime) -> Option<SystemTime> {
    match value {
        Value::Number(value) => number_date_time(value.as_f64()?, fetched_at),
        Value::String(value) => string_date_time(value, fetched_at),
        Value::Object(dictionary) => [
            "at",
            "time",
            "timestamp",
            "date",
            "value",
            "resetAt",
            "resetTime",
        ]
        .iter()
        .find_map(|key| {
            dictionary
                .get(*key)
                .and_then(|value| date_time(value, fetched_at))
        }),
        _ => None,
    }
}

fn number_date_time(value: f64, fetched_at: SystemTime) -> Option<SystemTime> {
    if !value.is_finite() {
        return None;
    }
    if value > 10_000_000_000.0 {
        return unix_time_from_seconds(value / 1_000.0);
    }
    if value > 1_000_000_000.0 {
        return unix_time_from_seconds(value);
    }
    if value >= 0.0 {
        return Some(fetched_at + Duration::from_secs_f64(value));
    }
    None
}

fn string_date_time(value: &str, fetched_at: SystemTime) -> Option<SystemTime> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(date) = DateTime::parse_from_rfc3339(trimmed) {
        return system_time_from_date_time(date);
    }
    parse_duration_seconds(trimmed).map(|seconds| fetched_at + Duration::from_secs(seconds as u64))
}

fn unix_time_from_seconds(seconds: f64) -> Option<SystemTime> {
    if seconds < 0.0 {
        return None;
    }
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs_f64(seconds))
}

fn system_time_from_date_time(date: DateTime<FixedOffset>) -> Option<SystemTime> {
    let seconds = date.timestamp();
    if seconds < 0 {
        return None;
    }
    Some(SystemTime::UNIX_EPOCH + Duration::new(seconds as u64, date.timestamp_subsec_nanos()))
}

fn duration_seconds(value: &Value) -> Option<i64> {
    match value {
        Value::Number(value) => value.as_i64().map(|seconds| seconds.max(0)),
        Value::String(value) => parse_duration_seconds(value),
        Value::Object(dictionary) => ["seconds", "second", "value", "duration"]
            .iter()
            .find_map(|key| dictionary.get(*key).and_then(duration_seconds)),
        _ => None,
    }
}

fn parse_duration_seconds(value: &str) -> Option<i64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(seconds) = trimmed.parse::<i64>() {
        return Some(seconds.max(0));
    }

    let mut total = 0.0;
    let mut number = String::new();
    let mut unit = String::new();
    for character in trimmed.chars().chain(std::iter::once(' ')) {
        if character.is_ascii_digit() || character == '.' {
            if !unit.is_empty() {
                total += duration_part_seconds(&number, &unit)?;
                number.clear();
                unit.clear();
            }
            number.push(character);
        } else if character.is_ascii_alphabetic() {
            unit.push(character);
        } else if !number.is_empty() && !unit.is_empty() {
            total += duration_part_seconds(&number, &unit)?;
            number.clear();
            unit.clear();
        }
    }

    (total > 0.0).then_some(total.round() as i64)
}

fn duration_part_seconds(number: &str, unit: &str) -> Option<f64> {
    let value = number.parse::<f64>().ok()?;
    let multiplier = match unit.to_lowercase().as_str() {
        "d" | "day" | "days" => 86_400.0,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3_600.0,
        "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
        "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
        _ => return None,
    };
    Some(value * multiplier)
}

fn contains_any_key(dictionary: &Map<String, Value>, keys: &[&str]) -> bool {
    keys.iter().any(|key| dictionary.contains_key(*key))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use std::time::{Duration, SystemTime};

    use super::{GrokRateLimit, GrokSubscriptionsResponse, GrokTypeaheadResponse};

    #[test]
    fn parses_typeahead_suggestions_like_swift() {
        let response = GrokTypeaheadResponse::from_raw_json(
            json!({
                "suggestions": [
                    { "text": "test driven development", "title": "TDD" },
                    { "query": "testing rust" },
                    "test cases",
                    { "text": "Test Cases" }
                ]
            }),
            3,
        );

        assert_eq!(
            response
                .suggestions
                .iter()
                .map(|suggestion| suggestion.text.as_str())
                .collect::<Vec<_>>(),
            vec!["test driven development", "testing rust", "test cases"]
        );
        assert_eq!(response.suggestions[0].title.as_deref(), Some("TDD"));
    }

    #[test]
    fn parses_subscriptions_and_current_display_name_like_swift() {
        let response = GrokSubscriptionsResponse::from_raw_json(json!({
            "subscriptions": [
                { "subscriptionTier": "TIER_SUPERGROK", "status": "canceled" },
                {
                    "subscriptionTier": "TIER_SUPERGROK_HEAVY",
                    "planName": "SuperGrok Heavy",
                    "status": "active"
                }
            ]
        }));

        assert_eq!(response.subscriptions.len(), 2);
        assert_eq!(
            response
                .current_subscription
                .as_ref()
                .and_then(|subscription| subscription.tier.as_deref()),
            Some("TIER_SUPERGROK_HEAVY")
        );
        assert_eq!(response.display_name(), "SuperGrok Heavy");
    }

    #[test]
    fn parses_nested_rate_limits_for_requested_model_like_swift() {
        let fetched_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let response = GrokRateLimit::from_raw_json(
            json!({
                "data": {
                    "rateLimits": [
                        {
                            "modelName": "other",
                            "remainingResponses": 99,
                            "resetAfterSeconds": 60
                        },
                        {
                            "modelName": "grok-420-computer-use-sa",
                            "responsesRemaining": "8",
                            "resetIn": "1h 30m"
                        }
                    ]
                }
            }),
            "grok-420-computer-use-sa",
        );

        assert_eq!(
            response.model_name.as_deref(),
            Some("grok-420-computer-use-sa")
        );
        assert_eq!(response.remaining_responses, Some(8));
        assert_eq!(response.reset_after_seconds, Some(5_400));
        assert!(response.is_low());

        let response = GrokRateLimit::from_raw_json_at(
            response.raw_json.clone(),
            "grok-420-computer-use-sa",
            fetched_at,
        );
        assert_eq!(
            response.seconds_until_reset(fetched_at + Duration::from_secs(400)),
            Some(5_000)
        );
    }

    #[test]
    fn parses_rate_limit_window_when_reset_is_missing() {
        let response = GrokRateLimit::from_raw_json(
            json!({
                "rateLimit": {
                    "modelName": "fast",
                    "remainingResponses": 4,
                    "window": "1h"
                }
            }),
            "fast",
        );

        assert_eq!(response.model_name.as_deref(), Some("fast"));
        assert_eq!(response.remaining_responses, Some(4));
        assert_eq!(response.reset_after_seconds, None);
        assert_eq!(response.window_seconds, Some(3_600));
    }

    #[test]
    fn parses_rate_limit_absolute_reset_times_like_swift() {
        let fetched_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let response = GrokRateLimit::from_raw_json_at(
            json!({
                "rateLimit": {
                    "modelName": "fast",
                    "remainingResponses": 5,
                    "resetAt": "1970-01-01T00:33:20Z"
                }
            }),
            "fast",
            fetched_at,
        );

        assert_eq!(response.model_name.as_deref(), Some("fast"));
        assert_eq!(response.remaining_responses, Some(5));
        assert_eq!(
            response.seconds_until_reset(SystemTime::UNIX_EPOCH + Duration::from_secs(1_300)),
            Some(700)
        );
        assert_eq!(
            response
                .seconds_until_reset(SystemTime::UNIX_EPOCH + Duration::new(1_299, 250_000_000)),
            Some(701)
        );
    }

    #[test]
    fn parses_nested_rate_limit_reset_time_values_like_swift() {
        let fetched_at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);
        let response = GrokRateLimit::from_raw_json_at(
            json!({
                "rateLimit": {
                    "modelName": "fast",
                    "remainingResponses": 5,
                    "reset": { "value": "15m" }
                }
            }),
            "fast",
            fetched_at,
        );

        assert_eq!(
            response.seconds_until_reset(SystemTime::UNIX_EPOCH + Duration::from_secs(1_300)),
            Some(600)
        );
    }
}
