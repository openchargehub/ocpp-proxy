use std::path::{Path, PathBuf};
use std::time::Duration;

use ocpp_proxy_application::SessionStore;
use ocpp_proxy_domain::SessionRow;
use rusqlite::{Connection, OptionalExtension};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sessions (
    timestamp TEXT,
    backend_id TEXT,
    duration_s REAL,
    energy_kwh REAL,
    revenue REAL,
    charger_id TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS chargers (
    id TEXT PRIMARY KEY,
    enabled INTEGER NOT NULL,
    ocpp_version TEXT NOT NULL,
    auto_detect INTEGER NOT NULL,
    allow_shared_charging INTEGER NOT NULL,
    preferred_provider TEXT NOT NULL,
    blocked_providers TEXT NOT NULL,
    allowed_providers TEXT NOT NULL,
    presence_sensor TEXT NOT NULL,
    override_input_boolean TEXT NOT NULL,
    rate_limit_seconds INTEGER NOT NULL,
    last_seen TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS charger_services (
    charger_id TEXT NOT NULL,
    id TEXT NOT NULL,
    url TEXT NOT NULL,
    version TEXT NOT NULL,
    auth_type TEXT NOT NULL,
    username TEXT NOT NULL,
    password TEXT NOT NULL,
    token TEXT NOT NULL,
    enabled INTEGER NOT NULL,
    PRIMARY KEY (charger_id, id)
)";

#[derive(Clone, Debug)]
pub struct SqliteStore {
    path: PathBuf,
}

impl SqliteStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let store = Self { path };
        store.with_conn(|conn| {
            conn.execute_batch(SCHEMA).map_err(|err| err.to_string())?;
            ensure_charger_id(conn)?;
            Ok(())
        })?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn with_conn<T>(
        &self,
        body: impl FnOnce(&Connection) -> Result<T, String>,
    ) -> Result<T, String> {
        let conn = Connection::open(&self.path).map_err(|err| {
            format!(
                "failed to open session database {}: {err}",
                self.path.display()
            )
        })?;
        conn.busy_timeout(Duration::from_secs(5)).map_err(|err| {
            format!(
                "failed to set busy timeout on {}: {err}",
                self.path.display()
            )
        })?;
        body(&conn)
    }
}

impl SessionStore for SqliteStore {
    fn insert(&self, row: &SessionRow) -> Result<(), String> {
        self.with_conn(|conn| {
            conn.execute(
                "INSERT INTO sessions (timestamp, backend_id, duration_s, energy_kwh, revenue, charger_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                (
                    &row.timestamp,
                    &row.backend_id,
                    row.duration_s,
                    row.energy_kwh,
                    row.revenue,
                    &row.charger_id,
                ),
            )
            .map_err(|err| format!("failed to insert session for {}: {err}", row.backend_id))?;
            Ok(())
        })
    }

    fn list(&self) -> Result<Vec<SessionRow>, String> {
        self.with_conn(|conn| {
            let mut statement = conn
                .prepare(
                    "SELECT timestamp, backend_id, duration_s, energy_kwh, revenue, IFNULL(charger_id, '') FROM sessions ORDER BY timestamp",
                )
                .map_err(|err| format!("failed to read sessions: {err}"))?;
            let rows = statement
                .query_map([], read_session)
                .map_err(|err| format!("failed to read sessions: {err}"))?;
            rows.collect::<Result<Vec<_>, _>>().map_err(|err| format!("failed to read sessions: {err}"))
        })
    }
}

impl SqliteStore {
    pub fn sessions_for(&self, charger_id: &str) -> Result<Vec<SessionRow>, String> {
        let charger_id = charger_id.to_string();
        self.with_conn(|conn| {
            let mut statement = conn
                .prepare(
                    "SELECT timestamp, backend_id, duration_s, energy_kwh, revenue, IFNULL(charger_id, '')
                     FROM sessions
                     WHERE charger_id = ?1
                        OR (?1 = 'default' AND IFNULL(charger_id, '') = '')
                     ORDER BY timestamp",
                )
                .map_err(|err| format!("failed to read sessions: {err}"))?;
            let rows = statement
                .query_map([&charger_id], read_session)
                .map_err(|err| format!("failed to read sessions: {err}"))?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|err| format!("failed to read sessions: {err}"))
        })
    }
}

fn read_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        timestamp: row.get(0)?,
        backend_id: row.get(1)?,
        duration_s: row.get(2)?,
        energy_kwh: row.get(3)?,
        revenue: row.get(4)?,
        charger_id: row.get(5)?,
    })
}

fn ensure_charger_id(conn: &Connection) -> Result<(), String> {
    if column_on(conn, "sessions", "charger_id")? {
        return Ok(());
    }
    conn.execute(
        "ALTER TABLE sessions ADD COLUMN charger_id TEXT NOT NULL DEFAULT ''",
        [],
    )
    .map_err(|err| format!("failed to add sessions.charger_id: {err}"))?;
    Ok(())
}

fn column_on(conn: &Connection, table: &str, column: &str) -> Result<bool, String> {
    let sql = format!("PRAGMA table_info({table})");
    let mut statement = conn
        .prepare(&sql)
        .map_err(|err| format!("failed to read {table} columns: {err}"))?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|err| format!("failed to read {table} columns: {err}"))?;
    for name in names {
        if name.map_err(|err| format!("failed to read {table} columns: {err}"))? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn column_names(path: &Path) -> Result<Vec<String>, String> {
    let store = SqliteStore {
        path: path.to_path_buf(),
    };
    store.with_conn(|conn| {
        let mut statement = conn
            .prepare("PRAGMA table_info(sessions)")
            .map_err(|err| err.to_string())?;
        let names = statement
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|err| err.to_string())?;
        names
            .collect::<Result<Vec<_>, _>>()
            .map_err(|err| err.to_string())
    })
}

pub fn table_exists(path: &Path) -> Result<bool, String> {
    let store = SqliteStore {
        path: path.to_path_buf(),
    };
    store.with_conn(|conn| {
        let found: Option<String> = conn
            .query_row(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'sessions'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|err| err.to_string())?;
        Ok(found.is_some())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ocpp_proxy_application::SessionStore;
    use std::thread;

    fn temp_db(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "ocpp-proxy-{name}-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn row(ts: &str, id: &str, energy: f64) -> SessionRow {
        SessionRow {
            timestamp: ts.into(),
            backend_id: id.into(),
            duration_s: energy,
            energy_kwh: energy,
            revenue: 0.0,
            charger_id: String::new(),
        }
    }

    #[test]
    fn schema_order_unicode_precision_and_reopen() {
        let path = temp_db("store");
        let store = SqliteStore::open(&path).unwrap();
        assert!(table_exists(&path).unwrap());
        assert_eq!(
            column_names(&path).unwrap(),
            vec![
                "timestamp",
                "backend_id",
                "duration_s",
                "energy_kwh",
                "revenue",
                "charger_id"
            ]
        );
        assert!(store.list().unwrap().is_empty());
        store
            .insert(&row("2023-01-01T12:00:00+00:00", "b", 0.0))
            .unwrap();
        store
            .insert(&SessionRow {
                timestamp: "2023-01-01T13:00:00+00:00".into(),
                backend_id: "充電".into(),
                duration_s: -1.5,
                energy_kwh: 1.23456789,
                revenue: -0.25,
                charger_id: String::new(),
            })
            .unwrap();
        store
            .insert(&row("2023-01-01T11:00:00+00:00", "a", 2.0))
            .unwrap();
        let rows = store.list().unwrap();
        assert_eq!(rows[0].backend_id, "a");
        assert_eq!(rows[1].energy_kwh, 0.0);
        assert_eq!(rows[2].backend_id, "充電");
        assert!((rows[2].energy_kwh - 1.23456789).abs() < 1e-6);
        assert_eq!(rows[2].duration_s, -1.5);
        drop(store);
        let reopened = SqliteStore::open(&path).unwrap();
        assert_eq!(reopened.list().unwrap().len(), 3);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn concurrent_inserts_and_missing_directory() {
        let path = temp_db("concurrent");
        let store = SqliteStore::open(&path).unwrap();
        let mut threads = Vec::new();
        for index in 0..8 {
            let store = store.clone();
            threads.push(thread::spawn(move || {
                store
                    .insert(&row(
                        &format!("2023-01-01T12:00:{index:02}+00:00"),
                        &format!("id-{index}"),
                        index as f64,
                    ))
                    .unwrap();
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(store.list().unwrap().len(), 8);
        let missing = SqliteStore::open("/tmp/ocpp-proxy-missing-dir/nope/usage.db");
        assert!(missing
            .unwrap_err()
            .contains("failed to open session database"));
        let _ = std::fs::remove_file(&path);
    }

    fn defaults(version: &str) -> ocpp_proxy_application::ChargerDefaults {
        ocpp_proxy_application::ChargerDefaults {
            ocpp_version: version.into(),
            auto_detect: true,
            allow_shared_charging: true,
            preferred_provider: "energy".into(),
            blocked_providers: vec!["spam".into()],
            allowed_providers: vec!["energy".into()],
            presence_sensor: "binary_sensor.home".into(),
            override_input_boolean: "input_boolean.override".into(),
            rate_limit_seconds: 10,
        }
    }

    #[test]
    fn enroll_keeps_the_first_row_and_services_stay_on_that_charger() {
        use ocpp_proxy_application::{ChargerRegistry, EnrollOutcome};

        let path = temp_db("registry");
        let store = SqliteStore::open(&path).unwrap();
        let created = store.enroll("CPCYBER01", &defaults("1.6")).unwrap();
        let EnrollOutcome::Created(row) = created else {
            panic!("first connect must insert the row");
        };
        assert!(row.enabled);
        assert_eq!(row.ocpp_version, "1.6");
        store.set_enabled("CPCYBER01", false).unwrap();
        let again = store.enroll("CPCYBER01", &defaults("2.0.1")).unwrap();
        let EnrollOutcome::Existing(kept) = again else {
            panic!("second enroll must not replace the row");
        };
        assert!(!kept.enabled);
        assert_eq!(kept.ocpp_version, "1.6");

        store
            .save_service(&ocpp_proxy_application::ServiceRecord {
                charger_id: "CPCYBER01".into(),
                id: "grid".into(),
                url: "ws://grid".into(),
                version: "1.6".into(),
                auth_type: "token".into(),
                username: String::new(),
                password: String::new(),
                token: "secret".into(),
                enabled: true,
            })
            .unwrap();
        assert!(store.list_services("CPVIARIO001").unwrap().is_empty());
        let services = store.list_services("CPCYBER01").unwrap();
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].token, "secret");
        store.delete_charger("CPCYBER01").unwrap();
        assert!(store.get_charger("CPCYBER01").unwrap().is_none());
        assert!(store.list_services("CPCYBER01").unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn old_session_rows_stay_readable_with_empty_charger_id() {
        use ocpp_proxy_application::SessionStore;
        use rusqlite::Connection;

        let path = temp_db("legacy-sessions");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                timestamp TEXT,
                backend_id TEXT,
                duration_s REAL,
                energy_kwh REAL,
                revenue REAL
            )",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (timestamp, backend_id, duration_s, energy_kwh, revenue) VALUES ('2020-01-01T00:00:00Z', 'old', 1.0, 2.0, 0.0)",
            [],
        )
        .unwrap();
        drop(conn);
        let store = SqliteStore::open(&path).unwrap();
        let rows = store.sessions_for("default").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].backend_id, "old");
        assert_eq!(rows[0].charger_id, "");
        assert!(store.sessions_for("CPCYBER01").unwrap().is_empty());
        store
            .insert(&SessionRow {
                timestamp: "2024-01-01T00:00:00Z".into(),
                backend_id: "energy".into(),
                duration_s: 3.0,
                energy_kwh: 4.0,
                revenue: 0.0,
                charger_id: "CPCYBER01".into(),
            })
            .unwrap();
        assert_eq!(store.sessions_for("CPCYBER01").unwrap().len(), 1);
        assert_eq!(store.sessions_for("default").unwrap().len(), 1);
        let _ = std::fs::remove_file(&path);
    }
}
