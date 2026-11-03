use ocpp_proxy_application::{
    ChargerDefaults, ChargerRecord, ChargerRegistry, EnrollOutcome, ServiceRecord,
};
use rusqlite::OptionalExtension;

use crate::store::SqliteStore;

impl ChargerRegistry for SqliteStore {
    fn list_chargers(&self) -> Result<Vec<ChargerRecord>, String> {
        self.with_conn(|conn| {
            let mut statement = conn
                .prepare(&format!(
                    "SELECT {CHARGER_COLUMNS} FROM chargers ORDER BY id"
                ))
                .map_err(|err| format!("failed to list chargers: {err}"))?;
            let rows = statement
                .query_map([], read_charger)
                .map_err(|err| format!("failed to list chargers: {err}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|err| format!("failed to list chargers: {err}"))
        })
    }

    fn get_charger(&self, id: &str) -> Result<Option<ChargerRecord>, String> {
        let id = id.to_string();
        self.with_conn(|conn| {
            conn.query_row(
                &format!("SELECT {CHARGER_COLUMNS} FROM chargers WHERE id = ?1"),
                [&id],
                read_charger,
            )
            .optional()
            .map_err(|err| format!("failed to read charger {id}: {err}"))
        })
    }

    fn enroll(&self, id: &str, defaults: &ChargerDefaults) -> Result<EnrollOutcome, String> {
        let id = id.to_string();
        self.with_conn(|conn| {
            let inserted = conn
                .execute(
                    "INSERT INTO chargers (
                        id, enabled, ocpp_version, auto_detect, allow_shared_charging,
                        preferred_provider, blocked_providers, allowed_providers,
                        presence_sensor, override_input_boolean, rate_limit_seconds, last_seen
                    ) VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, '')
                    ON CONFLICT(id) DO NOTHING",
                    (
                        &id,
                        &defaults.ocpp_version,
                        bool_int(defaults.auto_detect),
                        bool_int(defaults.allow_shared_charging),
                        &defaults.preferred_provider,
                        encode_list(&defaults.blocked_providers),
                        encode_list(&defaults.allowed_providers),
                        &defaults.presence_sensor,
                        &defaults.override_input_boolean,
                        defaults.rate_limit_seconds,
                    ),
                )
                .map_err(|err| format!("failed to enroll charger {id}: {err}"))?;
            let record = load_charger(conn, &id)?;
            if inserted == 1 {
                Ok(EnrollOutcome::Created(record))
            } else {
                Ok(EnrollOutcome::Existing(record))
            }
        })
    }

    fn update_charger(&self, record: &ChargerRecord) -> Result<(), String> {
        self.with_conn(|conn| {
            let changed = conn
                .execute(
                    "UPDATE chargers SET
                        ocpp_version = ?2,
                        auto_detect = ?3,
                        allow_shared_charging = ?4,
                        preferred_provider = ?5,
                        blocked_providers = ?6,
                        allowed_providers = ?7,
                        presence_sensor = ?8,
                        override_input_boolean = ?9,
                        rate_limit_seconds = ?10
                     WHERE id = ?1",
                    (
                        &record.id,
                        &record.ocpp_version,
                        bool_int(record.auto_detect),
                        bool_int(record.allow_shared_charging),
                        &record.preferred_provider,
                        encode_list(&record.blocked_providers),
                        encode_list(&record.allowed_providers),
                        &record.presence_sensor,
                        &record.override_input_boolean,
                        record.rate_limit_seconds,
                    ),
                )
                .map_err(|err| format!("failed to update charger {}: {err}", record.id))?;
            missing(changed, &record.id)
        })
    }

    fn set_enabled(&self, id: &str, enabled: bool) -> Result<(), String> {
        let id = id.to_string();
        self.with_conn(|conn| {
            let changed = conn
                .execute(
                    "UPDATE chargers SET enabled = ?2 WHERE id = ?1",
                    (&id, bool_int(enabled)),
                )
                .map_err(|err| format!("failed to set enabled on charger {id}: {err}"))?;
            missing(changed, &id)
        })
    }

    fn delete_charger(&self, id: &str) -> Result<(), String> {
        let id = id.to_string();
        self.with_conn(|conn| {
            conn.execute("DELETE FROM charger_services WHERE charger_id = ?1", [&id])
                .map_err(|err| format!("failed to delete services for charger {id}: {err}"))?;
            let changed = conn
                .execute("DELETE FROM chargers WHERE id = ?1", [&id])
                .map_err(|err| format!("failed to delete charger {id}: {err}"))?;
            missing(changed, &id)
        })
    }

    fn list_services(&self, charger_id: &str) -> Result<Vec<ServiceRecord>, String> {
        let charger_id = charger_id.to_string();
        self.with_conn(|conn| {
            let mut statement = conn
                .prepare(
                    "SELECT charger_id, id, url, version, auth_type, username, password, token, enabled
                     FROM charger_services WHERE charger_id = ?1 ORDER BY id",
                )
                .map_err(|err| format!("failed to list services for {charger_id}: {err}"))?;
            let rows = statement
                .query_map([&charger_id], read_service)
                .map_err(|err| format!("failed to list services for {charger_id}: {err}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|err| format!("failed to list services for {charger_id}: {err}"))
        })
    }

    fn save_service(&self, service: &ServiceRecord) -> Result<(), String> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO charger_services (
                    charger_id, id, url, version, auth_type, username, password, token, enabled
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(charger_id, id) DO UPDATE SET
                    url = excluded.url,
                    version = excluded.version,
                    auth_type = excluded.auth_type,
                    username = excluded.username,
                    password = excluded.password,
                    token = excluded.token,
                    enabled = excluded.enabled",
                (
                    &service.charger_id,
                    &service.id,
                    &service.url,
                    &service.version,
                    &service.auth_type,
                    &service.username,
                    &service.password,
                    &service.token,
                    bool_int(service.enabled),
                ),
            )
            .map_err(|err| {
                format!(
                    "failed to save service {} for charger {}: {err}",
                    service.id, service.charger_id
                )
            })?;
            Ok(())
        })
    }

    fn delete_service(&self, charger_id: &str, service_id: &str) -> Result<(), String> {
        let charger_id = charger_id.to_string();
        let service_id = service_id.to_string();
        self.with_conn(|conn| {
            let changed = conn
                .execute(
                    "DELETE FROM charger_services WHERE charger_id = ?1 AND id = ?2",
                    (&charger_id, &service_id),
                )
                .map_err(|err| {
                    format!("failed to delete service {service_id} for charger {charger_id}: {err}")
                })?;
            if changed == 0 {
                return Err(format!(
                    "service {service_id} was not found on charger {charger_id}"
                ));
            }
            Ok(())
        })
    }

    fn touch_last_seen(&self, id: &str, seen: &str) -> Result<(), String> {
        let id = id.to_string();
        let seen = seen.to_string();
        self.with_conn(|conn| {
            conn.execute(
                "UPDATE chargers SET last_seen = ?2 WHERE id = ?1",
                (&id, &seen),
            )
            .map_err(|err| format!("failed to stamp last seen for charger {id}: {err}"))?;
            Ok(())
        })
    }
}

const CHARGER_COLUMNS: &str = "id, enabled, ocpp_version, auto_detect, allow_shared_charging, preferred_provider, blocked_providers, allowed_providers, presence_sensor, override_input_boolean, rate_limit_seconds, last_seen";

fn load_charger(conn: &rusqlite::Connection, id: &str) -> Result<ChargerRecord, String> {
    conn.query_row(
        &format!("SELECT {CHARGER_COLUMNS} FROM chargers WHERE id = ?1"),
        [id],
        read_charger,
    )
    .map_err(|err| format!("failed to read charger {id}: {err}"))
}

fn read_charger(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChargerRecord> {
    Ok(ChargerRecord {
        id: row.get(0)?,
        enabled: row.get::<_, i64>(1)? != 0,
        ocpp_version: row.get(2)?,
        auto_detect: row.get::<_, i64>(3)? != 0,
        allow_shared_charging: row.get::<_, i64>(4)? != 0,
        preferred_provider: row.get(5)?,
        blocked_providers: decode_list(&row.get::<_, String>(6)?),
        allowed_providers: decode_list(&row.get::<_, String>(7)?),
        presence_sensor: row.get(8)?,
        override_input_boolean: row.get(9)?,
        rate_limit_seconds: row.get(10)?,
        last_seen: row.get(11)?,
    })
}

fn read_service(row: &rusqlite::Row<'_>) -> rusqlite::Result<ServiceRecord> {
    Ok(ServiceRecord {
        charger_id: row.get(0)?,
        id: row.get(1)?,
        url: row.get(2)?,
        version: row.get(3)?,
        auth_type: row.get(4)?,
        username: row.get(5)?,
        password: row.get(6)?,
        token: row.get(7)?,
        enabled: row.get::<_, i64>(8)? != 0,
    })
}

fn missing(changed: usize, id: &str) -> Result<(), String> {
    if changed == 0 {
        return Err(format!("charger {id} was not found"));
    }
    Ok(())
}

fn bool_int(value: bool) -> i64 {
    if value {
        1
    } else {
        0
    }
}

fn encode_list(items: &[String]) -> String {
    serde_json::to_string(items).unwrap_or_else(|_| "[]".to_string())
}

fn decode_list(text: &str) -> Vec<String> {
    serde_json::from_str(text).unwrap_or_default()
}
