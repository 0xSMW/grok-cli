use serde_json::{Map, Number, Value};

#[derive(Clone, Debug)]
pub struct JsonLookup {
    value: Value,
}

impl JsonLookup {
    pub fn new(value: Value) -> Self {
        Self { value }
    }

    pub fn raw(&self) -> &Value {
        &self.value
    }

    pub fn string(&self, keys: &[&str]) -> Option<String> {
        self.string_allowing_empty(keys)
            .filter(|value| !value.is_empty())
    }

    pub fn string_allowing_empty(&self, keys: &[&str]) -> Option<String> {
        self.first_direct_value(keys).and_then(string_value)
    }

    pub fn bool(&self, keys: &[&str]) -> Option<bool> {
        self.first_direct_value(keys).and_then(bool_value)
    }

    pub fn int(&self, keys: &[&str]) -> Option<i64> {
        self.first_direct_value(keys).and_then(int_value)
    }

    pub fn double(&self, keys: &[&str]) -> Option<f64> {
        self.first_direct_value(keys).and_then(double_value)
    }

    pub fn first_string(&self, keys: &[&str]) -> Option<String> {
        first_value(keys, &self.value)
            .and_then(string_value)
            .filter(|value| !value.is_empty())
    }

    pub fn first_bool(&self, keys: &[&str]) -> Option<bool> {
        first_value(keys, &self.value).and_then(bool_value)
    }

    pub fn first_int(&self, keys: &[&str]) -> Option<i64> {
        first_value(keys, &self.value).and_then(int_value)
    }

    pub fn first_dictionary(&self, keys: &[&str]) -> Option<Map<String, Value>> {
        self.dictionaries(keys).into_iter().next()
    }

    pub fn dictionaries(&self, keys: &[&str]) -> Vec<Map<String, Value>> {
        if let Some(array) = self.value.as_array() {
            return array.iter().flat_map(direct_dictionaries).collect();
        }

        let Some(dictionary) = self.value.as_object() else {
            return Vec::new();
        };

        for key in keys {
            if let Some(nested) = dictionary.get(*key) {
                let nested_dictionaries = direct_dictionaries(nested);
                if !nested_dictionaries.is_empty() {
                    return nested_dictionaries;
                }
            }
        }

        for key in keys {
            if let Some(nested) = dictionary.get(*key) {
                let nested_dictionaries = JsonLookup::new(nested.clone()).dictionaries(keys);
                if !nested_dictionaries.is_empty() {
                    return nested_dictionaries;
                }
            }
        }

        Vec::new()
    }

    pub fn all_dictionaries(&self, keys: &[&str]) -> Vec<Map<String, Value>> {
        all_dictionaries(&self.value, keys)
    }

    pub fn contains_any_key(&self, keys: &[&str]) -> bool {
        self.value
            .as_object()
            .is_some_and(|dictionary| keys.iter().any(|key| dictionary.contains_key(*key)))
    }

    fn first_direct_value(&self, keys: &[&str]) -> Option<Value> {
        let dictionary = self.value.as_object()?;
        keys.iter()
            .find_map(|key| dictionary.get(*key).map(ToOwned::to_owned))
    }
}

fn first_value(keys: &[&str], value: &Value) -> Option<Value> {
    match value {
        Value::Object(dictionary) => {
            for key in keys {
                if let Some(value) = dictionary.get(*key) {
                    return Some(value.clone());
                }
            }

            dictionary
                .values()
                .find_map(|nested| first_value(keys, nested))
        }
        Value::Array(values) => values.iter().find_map(|nested| first_value(keys, nested)),
        _ => None,
    }
}

fn direct_dictionaries(value: &Value) -> Vec<Map<String, Value>> {
    match value {
        Value::Array(values) => values.iter().flat_map(direct_dictionaries).collect(),
        Value::Object(dictionary) => vec![dictionary.clone()],
        _ => Vec::new(),
    }
}

fn all_dictionaries(value: &Value, keys: &[&str]) -> Vec<Map<String, Value>> {
    match value {
        Value::Array(values) => values
            .iter()
            .flat_map(|nested| all_dictionaries(nested, keys))
            .collect(),
        Value::Object(dictionary) => {
            let mut results = Vec::new();
            for (key, nested) in dictionary {
                if keys.contains(&key.as_str()) {
                    results.extend(direct_dictionaries(nested));
                } else {
                    results.extend(all_dictionaries(nested, keys));
                }
            }
            results
        }
        _ => Vec::new(),
    }
}

fn string_value(value: Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value),
        _ => None,
    }
}

fn bool_value(value: Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(value),
        Value::Number(value) => number_to_f64(&value).map(|number| number != 0.0),
        Value::String(value) => match value.to_lowercase().as_str() {
            "true" | "yes" | "1" => Some(true),
            "false" | "no" | "0" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn int_value(value: Value) -> Option<i64> {
    match value {
        Value::Bool(value) => Some(i64::from(value)),
        Value::Number(value) => value
            .as_i64()
            .or_else(|| value.as_f64().map(|number| number as i64)),
        Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

fn double_value(value: Value) -> Option<f64> {
    match value {
        Value::Bool(value) => Some(if value { 1.0 } else { 0.0 }),
        Value::Number(value) => number_to_f64(&value),
        Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

fn number_to_f64(value: &Number) -> Option<f64> {
    value.as_f64()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::JsonLookup;

    #[test]
    fn direct_scalar_lookup_does_not_recurse() {
        let lookup = JsonLookup::new(json!({
            "wrapper": {
                "target": "nested",
                "count": "7",
                "enabled": "true"
            }
        }));

        assert_eq!(lookup.string(&["target"]), None);
        assert_eq!(lookup.int(&["count"]), None);
        assert_eq!(lookup.bool(&["enabled"]), None);
    }

    #[test]
    fn recursive_scalar_lookup_finds_nested_values() {
        let lookup = JsonLookup::new(json!({
            "wrapper": {
                "items": [
                    {
                        "target": "nested",
                        "count": "7",
                        "enabled": "true"
                    }
                ]
            }
        }));

        assert_eq!(lookup.first_string(&["target"]).as_deref(), Some("nested"));
        assert_eq!(lookup.first_int(&["count"]), Some(7));
        assert_eq!(lookup.first_bool(&["enabled"]), Some(true));
    }

    #[test]
    fn all_dictionaries_traverses_wrappers_and_arrays() {
        let lookup = JsonLookup::new(json!({
            "envelope": {
                "result": {
                    "items": [
                        {"id": "one"},
                        {"id": "two"}
                    ]
                }
            }
        }));

        assert!(lookup.dictionaries(&["items"]).is_empty());
        let recursive = lookup.all_dictionaries(&["items"]);
        assert_eq!(recursive.len(), 2);
        assert_eq!(recursive[0].get("id"), Some(&json!("one")));
        assert_eq!(recursive[1].get("id"), Some(&json!("two")));
    }
}
