use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use serde_json::{Map, Value};

/// Why a config file or field could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    Read { path: String, detail: String },
    Parse(String),
    InvalidInteger(String),
}

impl fmt::Display for ConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, detail } => {
                write!(formatter, "failed to read config {path}: {detail}")
            }
            Self::Parse(detail) => write!(formatter, "failed to parse config: {detail}"),
            Self::InvalidInteger(value) => {
                write!(formatter, "invalid integer for rate_limit_seconds: {value}")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// One outbound OCPP service from the options file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcppServiceConfig {
    pub id: String,
    pub url: String,
    pub version: Option<String>,
    pub auth_type: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub token: Option<String>,
    pub enabled: bool,
}

impl OcppServiceConfig {
    /// Configured OCPP version, or `1.6` when the key is absent.
    pub fn version(&self) -> &str {
        self.version.as_deref().unwrap_or("1.6")
    }
}

/// Add-on options. Values stay raw until read so invalid integers fail on access.
#[derive(Debug, Clone, PartialEq)]
pub struct ProxyConfig {
    map: Map<String, Value>,
}

impl ProxyConfig {
    pub fn defaults() -> Self {
        Self { map: Map::new() }
    }

    /// Load YAML or JSON. A missing or empty file yields defaults.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        match fs::read_to_string(path) {
            Ok(text) => Self::from_document(&text),
            Err(err) => missing_or_unreadable(path, err),
        }
    }

    /// `ADDON_CONFIG_FILE`, or `/data/options.yaml` when the variable is unset.
    pub fn load_default() -> Result<Self, ConfigError> {
        let path =
            std::env::var("ADDON_CONFIG_FILE").unwrap_or_else(|_| "/data/options.yaml".to_string());
        Self::load(path)
    }

    pub fn from_document(text: &str) -> Result<Self, ConfigError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(Self::defaults());
        }
        let value = parse_value(trimmed)?;
        let map = match value {
            Value::Null => Map::new(),
            Value::Object(map) => map,
            _ => {
                return Err(ConfigError::Parse(
                    "config root must be a mapping".to_string(),
                ))
            }
        };
        Ok(Self { map })
    }

    pub fn allow_shared_charging(&self) -> bool {
        coerce_bool(self.map.get("allow_shared_charging"), false)
    }

    pub fn auto_detect_ocpp_version(&self) -> bool {
        coerce_bool(self.map.get("auto_detect_ocpp_version"), true)
    }

    pub fn preferred_provider(&self) -> String {
        coerce_string(self.map.get("preferred_provider"), "")
    }

    pub fn presence_sensor(&self) -> String {
        coerce_string(self.map.get("presence_sensor"), "")
    }

    pub fn override_input_boolean(&self) -> String {
        coerce_string(self.map.get("override_input_boolean"), "")
    }

    pub fn ocpp_version(&self) -> String {
        coerce_string(self.map.get("ocpp_version"), "1.6")
    }

    pub fn blocked_providers(&self) -> Vec<String> {
        if self.map.contains_key("blocked_providers") {
            coerce_string_list(self.map.get("blocked_providers"))
        } else {
            coerce_string_list(self.map.get("disallowed_providers"))
        }
    }

    pub fn disallowed_providers(&self) -> Vec<String> {
        self.blocked_providers()
    }

    pub fn allowed_providers(&self) -> Vec<String> {
        coerce_string_list(self.map.get("allowed_providers"))
    }

    pub fn rate_limit_seconds(&self) -> Result<i64, ConfigError> {
        coerce_int(self.map.get("rate_limit_seconds"), 10)
    }

    pub fn ocpp_services(&self) -> Vec<OcppServiceConfig> {
        match self.map.get("ocpp_services") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items.iter().filter_map(parse_service).collect(),
            Some(_) => Vec::new(),
        }
    }
}

fn parse_value(text: &str) -> Result<Value, ConfigError> {
    if text.starts_with('{') || text.starts_with('[') {
        return serde_json::from_str(text).map_err(|err| ConfigError::Parse(err.to_string()));
    }
    let yaml: serde_yaml::Value =
        serde_yaml::from_str(text).map_err(|err| ConfigError::Parse(err.to_string()))?;
    Ok(yaml_to_json(yaml))
}

fn yaml_to_json(value: serde_yaml::Value) -> Value {
    match value {
        serde_yaml::Value::Null => Value::Null,
        serde_yaml::Value::Bool(flag) => Value::Bool(flag),
        serde_yaml::Value::Number(number) => yaml_number(number),
        serde_yaml::Value::String(text) => Value::String(text),
        serde_yaml::Value::Sequence(items) => {
            Value::Array(items.into_iter().map(yaml_to_json).collect())
        }
        serde_yaml::Value::Mapping(map) => Value::Object(yaml_map(map)),
        serde_yaml::Value::Tagged(tagged) => yaml_to_json(tagged.value),
    }
}

fn yaml_number(number: serde_yaml::Number) -> Value {
    if let Some(value) = number.as_i64() {
        return Value::from(value);
    }
    if let Some(value) = number.as_u64() {
        return Value::from(value);
    }
    Value::from(number.to_string().parse::<f64>().unwrap_or(f64::NAN))
}

fn yaml_map(map: serde_yaml::Mapping) -> Map<String, Value> {
    map.into_iter()
        .map(|(key, value)| (yaml_key(key), yaml_to_json(value)))
        .collect()
}

fn yaml_key(key: serde_yaml::Value) -> String {
    match key {
        serde_yaml::Value::String(text) => text,
        other => yaml_to_json(other).to_string(),
    }
}

fn parse_service(value: &Value) -> Option<OcppServiceConfig> {
    let object = value.as_object()?;
    Some(OcppServiceConfig {
        id: coerce_string(object.get("id"), ""),
        url: coerce_string(object.get("url"), ""),
        version: optional_string(object.get("version")),
        auth_type: optional_string(object.get("auth_type")),
        username: optional_string(object.get("username")),
        password: optional_string(object.get("password")),
        token: optional_string(object.get("token")),
        enabled: match object.get("enabled") {
            None => true,
            Some(value) => coerce_bool(Some(value), true),
        },
    })
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    match value {
        None | Some(Value::Null) => None,
        Some(value) => Some(coerce_string(Some(value), "")),
    }
}

fn coerce_bool(value: Option<&Value>, default: bool) -> bool {
    match value {
        None => default,
        Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number_is_truthy(number),
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => false,
    }
}

fn missing_or_unreadable(path: &Path, err: std::io::Error) -> Result<ProxyConfig, ConfigError> {
    if err.kind() == ErrorKind::NotFound {
        return Ok(ProxyConfig::defaults());
    }
    Err(ConfigError::Read {
        path: path.display().to_string(),
        detail: err.to_string(),
    })
}

fn number_is_truthy(number: &serde_json::Number) -> bool {
    if let Some(value) = number.as_i64() {
        return value != 0;
    }
    number.as_f64().is_some_and(|value| value != 0.0)
}

fn coerce_string(value: Option<&Value>, default: &str) -> String {
    match value {
        None => default.to_string(),
        Some(Value::Null) => "None".to_string(),
        Some(Value::Bool(true)) => "True".to_string(),
        Some(Value::Bool(false)) => "False".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

fn coerce_int(value: Option<&Value>, default: i64) -> Result<i64, ConfigError> {
    match value {
        None => Ok(default),
        Some(Value::Number(number)) => number
            .as_i64()
            .ok_or_else(|| ConfigError::InvalidInteger(number.to_string())),
        Some(Value::String(text)) => text
            .parse()
            .map_err(|_| ConfigError::InvalidInteger(text.clone())),
        Some(other) => Err(ConfigError::InvalidInteger(other.to_string())),
    }
}

fn coerce_string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| coerce_string(Some(item), ""))
            .collect(),
        Some(_) => Vec::new(),
    }
}

#[cfg(test)]
mod coverage {
    use super::yaml_to_json;
    use serde_json::Value;
    use serde_yaml::value::{Tag, TaggedValue};
    use serde_yaml::Value as Yaml;

    #[test]
    fn non_finite_number_stays_text() {
        let from_float = serde_yaml::Number::from(f64::NAN);
        assert!(matches!(
            super::yaml_number(from_float),
            Value::Null | Value::String(_)
        ));
        let parsed: Yaml = serde_yaml::from_str(".nan").unwrap();
        let Yaml::Number(number) = parsed else {
            panic!("expected a number")
        };
        assert!(matches!(
            super::yaml_number(number),
            Value::Null | Value::String(_)
        ));
    }

    #[test]
    fn tagged_yaml_keeps_the_inner_value() {
        let value = Yaml::Tagged(Box::new(TaggedValue {
            tag: Tag::new("keep"),
            value: Yaml::String("kept".into()),
        }));
        assert_eq!(yaml_to_json(value), Value::String("kept".into()));
    }
}
