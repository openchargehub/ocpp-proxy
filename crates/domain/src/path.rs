use std::fmt;

/// Station used by `/charger` and `/backend` when no charger id is present.
pub const DEFAULT_STATION_ID: &str = "default";

const RESERVED_VERSIONS: &[&str] = &["v1.6", "v2.0", "v2.0.1"];

/// Which socket a `/charger...` path opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StationRoute {
    ChargePoint { id: String },
    Provider { id: String },
}

/// A path segment that is not a charger id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathError {
    pub segment: String,
}

impl fmt::Display for PathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "invalid charger id '{}': 1 to 48 ASCII letters, digits, '_' or '-'",
            self.segment
        )
    }
}

impl std::error::Error for PathError {}

/// `/charger`, `/charger/{id}`, and `/charger/{id}/backend`.
///
/// Reserved version suffixes (`v1.6`, `v2.0`, `v2.0.1`) stay on [`DEFAULT_STATION_ID`].
pub fn parse_station_path(path: &str) -> Result<StationRoute, PathError> {
    let path = path.split('?').next().unwrap_or(path);
    let path = path.trim_end_matches('/');
    let Some(rest) = path.strip_prefix("/charger") else {
        return Err(PathError {
            segment: path.to_string(),
        });
    };
    if rest.is_empty() {
        return Ok(charge_point(DEFAULT_STATION_ID));
    }
    let Some(rest) = rest.strip_prefix('/') else {
        return Err(PathError {
            segment: path.to_string(),
        });
    };
    let mut parts = rest.split('/');
    let first = parts.next().unwrap_or("");
    let second = parts.next();
    if parts.next().is_some() {
        return Err(PathError {
            segment: rest.to_string(),
        });
    }
    let id = station_id(first)?;
    match second {
        None => Ok(charge_point(&id)),
        Some("backend") => Ok(StationRoute::Provider { id }),
        Some(other) => Err(PathError {
            segment: other.to_string(),
        }),
    }
}

/// `true` when `id` matches the charger id rule and is not a reserved version token.
pub fn validate_charger_id(id: &str) -> Result<(), PathError> {
    if id_chars_ok(id) && !is_reserved_version(id) {
        return Ok(());
    }
    Err(PathError {
        segment: id.to_string(),
    })
}

/// Path passed to version detection. Named ids are omitted so the id is never a version.
pub fn version_hint_path(path: &str) -> Option<String> {
    let route = parse_station_path(path).ok()?;
    let StationRoute::ChargePoint { id } = route else {
        return None;
    };
    if id == DEFAULT_STATION_ID {
        Some(path.split('?').next().unwrap_or(path).to_string())
    } else {
        None
    }
}

fn station_id(segment: &str) -> Result<String, PathError> {
    if is_reserved_version(segment) {
        return Ok(DEFAULT_STATION_ID.to_string());
    }
    validate_charger_id(segment)?;
    Ok(segment.to_string())
}

fn charge_point(id: &str) -> StationRoute {
    StationRoute::ChargePoint { id: id.to_string() }
}

fn id_chars_ok(id: &str) -> bool {
    let len = id.len();
    (1..=48).contains(&len)
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

fn is_reserved_version(id: &str) -> bool {
    RESERVED_VERSIONS
        .iter()
        .any(|token| token.eq_ignore_ascii_case(id))
}
