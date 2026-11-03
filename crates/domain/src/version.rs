use std::fmt;

/// OCPP versions this proxy speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcppVersion {
    V16,
    V201,
}

impl OcppVersion {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::V16 => "1.6",
            Self::V201 => "2.0.1",
        }
    }

    pub fn subprotocol(self) -> &'static str {
        match self {
            Self::V16 => "ocpp1.6",
            Self::V201 => "ocpp2.0.1",
        }
    }
}

/// Hints taken from the charger WebSocket handshake.
#[derive(Debug, Clone, Default)]
pub struct ConnectionHints {
    pub subprotocol: Option<String>,
    pub headers: Vec<(String, String)>,
    pub path: Option<String>,
    pub query: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionError {
    pub version: String,
}

impl fmt::Display for VersionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "unsupported OCPP version: {}", self.version)
    }
}

impl std::error::Error for VersionError {}

pub fn supported_versions() -> &'static [&'static str] {
    &["1.6", "2.0.1"]
}

pub fn is_version_supported(version: &str) -> bool {
    supported_versions().contains(&version)
}

/// First hint wins: subprotocol, headers, path, then query.
pub fn detect_version(hints: &ConnectionHints) -> Option<OcppVersion> {
    if let Some(version) = hints.subprotocol.as_deref().and_then(from_subprotocol) {
        return Some(version);
    }
    if let Some(version) = from_headers(&hints.headers) {
        return Some(version);
    }
    if let Some(version) = hints.path.as_deref().and_then(from_path) {
        return Some(version);
    }
    from_query(&hints.query)
}

/// Auto-detect overrides the configured version. Otherwise the configured string is exact.
pub fn resolve_version(
    configured: &str,
    auto_detect: bool,
    hints: &ConnectionHints,
) -> Result<OcppVersion, VersionError> {
    if auto_detect {
        if let Some(detected) = detect_version(hints) {
            return Ok(detected);
        }
    }
    parse_exact(configured.trim())
}

fn parse_exact(version: &str) -> Result<OcppVersion, VersionError> {
    match version {
        "1.6" => Ok(OcppVersion::V16),
        "2.0.1" => Ok(OcppVersion::V201),
        other => Err(VersionError {
            version: other.to_string(),
        }),
    }
}

fn from_subprotocol(value: &str) -> Option<OcppVersion> {
    let lower = value.to_ascii_lowercase();
    if lower.contains("ocpp1.6") {
        return Some(OcppVersion::V16);
    }
    if lower.contains("ocpp2.0.1") || lower.contains("ocpp2.0") {
        return Some(OcppVersion::V201);
    }
    None
}

fn from_headers(headers: &[(String, String)]) -> Option<OcppVersion> {
    let websocket = header(headers, "Sec-WebSocket-Protocol");
    if let Some(version) = from_loose(websocket) {
        return Some(version);
    }
    let custom = {
        let named = header(headers, "X-OCPP-Version");
        if named.is_empty() {
            header(headers, "OCPP-Version")
        } else {
            named
        }
    };
    if custom.is_empty() {
        return None;
    }
    from_loose(custom)
}

fn from_loose(value: &str) -> Option<OcppVersion> {
    if value.contains("1.6") {
        return Some(OcppVersion::V16);
    }
    if value.contains("2.0.1") || value.contains("2.0") {
        return Some(OcppVersion::V201);
    }
    None
}

fn from_path(path: &str) -> Option<OcppVersion> {
    let path = path.to_ascii_lowercase();
    if path.contains("v1.6") || path.contains("ocpp16") || path.contains("1.6") {
        return Some(OcppVersion::V16);
    }
    if path.contains("v2.0.1") || path.contains("ocpp201") || path.contains("2.0.1") {
        return Some(OcppVersion::V201);
    }
    if path.contains("v2.0") || path.contains("ocpp20") {
        return Some(OcppVersion::V201);
    }
    None
}

fn from_query(query: &[(String, String)]) -> Option<OcppVersion> {
    let version = query_value(query, "version");
    let version = if version.is_empty() {
        query_value(query, "ocpp_version")
    } else {
        version
    };
    if version.is_empty() {
        return None;
    }
    from_loose(version)
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
        .unwrap_or("")
}

fn query_value<'a>(query: &'a [(String, String)], name: &str) -> &'a str {
    query
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
        .unwrap_or("")
}
