use ocpp_proxy_application::{ChargerRecord, ServiceRecord};
use ocpp_proxy_domain::SessionRow;

use crate::state::StationHandle;

pub struct ListedCharger {
    pub id: String,
    pub connected: bool,
    pub version: String,
    pub lock_owner: String,
    pub inbound: usize,
    pub outbound: usize,
    pub last_seen: String,
}

pub fn charger_list_page(rows: &[ListedCharger]) -> String {
    let mut body = String::from(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>EV Charger Proxy</title>
</head>
<body>
  <h1>EV Charger Proxy</h1>
  <p>One process, one row per charger. Detail pages need an admin token.</p>
  <table>
    <thead>
      <tr><th>Id</th><th>Connected</th><th>OCPP</th><th>Lock</th><th>Inbound</th><th>Outbound</th><th>Last seen</th></tr>
    </thead>
    <tbody>
"#,
    );
    for row in rows {
        let connected = if row.connected { "yes" } else { "no" };
        let owner = if row.lock_owner.is_empty() {
            "—"
        } else {
            row.lock_owner.as_str()
        };
        let seen = if row.last_seen.is_empty() {
            "—"
        } else {
            row.last_seen.as_str()
        };
        body.push_str(&format!(
            "<tr><td><a href=\"/chargers/{id}\">{id}</a></td><td>{connected}</td><td>{version}</td><td>{owner}</td><td>{inbound}</td><td>{outbound}</td><td>{seen}</td></tr>\n",
            id = esc(&row.id),
            version = esc(&row.version),
            owner = esc(owner),
            inbound = row.inbound,
            outbound = row.outbound,
            seen = esc(seen),
        ));
    }
    body.push_str(
        r#"    </tbody>
  </table>
  <h2>Add a charger</h2>
  <form method="post" action="/chargers">
    <label>Id <input name="id" required></label>
    <label>Admin token <input name="token"></label>
    <button type="submit">Add</button>
  </form>
  <h2>Endpoints</h2>
  <ul>
    <li><a href="/charger">/charger</a> (WebSocket for the default charger)</li>
    <li><a href="/backend?id=your_backend_id">/backend?id=your_backend_id</a> (WebSocket for a default-station backend)</li>
    <li><a href="/sessions">/sessions</a> (JSON session data)</li>
    <li><a href="/sessions.csv">/sessions.csv</a> (CSV session data)</li>
    <li><a href="/status">/status</a> (backend status and control owner)</li>
    <li><a href="/override">/override</a> (POST to override control owner)</li>
  </ul>
</body>
</html>
"#,
    );
    body
}

pub fn charger_detail_page(
    record: &ChargerRecord,
    station: Option<&StationHandle>,
    services: &[ServiceRecord],
    sessions: &[SessionRow],
    token: &str,
) -> String {
    let (connected, version, owner, inbound) = live_bits(station, record);
    let (vendor, model) = station
        .and_then(StationHandle::boot)
        .unwrap_or_else(|| (String::new(), String::new()));
    let mut body = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Charger {id}</title>
</head>
<body>
  <p><a href="/">All chargers</a></p>
  <h1>Charger {id}</h1>
  <p>Connected: {connected}. OCPP {version}. Lock: {owner}. Last seen: {seen}.</p>
  <p>Boot vendor: {vendor}. Model: {model}.</p>
  <h2>Inbound providers</h2>
  <ul>
"#,
        id = esc(&record.id),
        connected = if connected { "yes" } else { "no" },
        version = esc(&version),
        owner = esc(&owner),
        seen = esc(if record.last_seen.is_empty() {
            "—"
        } else {
            record.last_seen.as_str()
        }),
        vendor = esc(&vendor),
        model = esc(&model),
    );
    if inbound.is_empty() {
        body.push_str("<li>none</li>\n");
    }
    for name in &inbound {
        body.push_str(&format!("<li>{}</li>\n", esc(name)));
    }
    body.push_str("</ul>\n<h2>Outbound services</h2>\n");
    for service in services {
        body.push_str(&service_form(&record.id, service, token));
    }
    body.push_str(&service_form(
        &record.id,
        &ServiceRecord {
            charger_id: record.id.clone(),
            id: String::new(),
            url: String::new(),
            version: "1.6".into(),
            auth_type: String::new(),
            username: String::new(),
            password: String::new(),
            token: String::new(),
            enabled: true,
        },
        token,
    ));
    body.push_str(&policy_form(record, token));
    body.push_str("<h2>Sessions</h2><table><thead><tr><th>When</th><th>Provider</th><th>kWh</th><th>Seconds</th></tr></thead><tbody>\n");
    for row in sessions {
        body.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>\n",
            esc(&row.timestamp),
            esc(&row.backend_id),
            row.energy_kwh,
            row.duration_s
        ));
    }
    body.push_str("</tbody></table>\n");
    body.push_str(&action_form(
        &record.id,
        token,
        if record.enabled { "disable" } else { "enable" },
        if record.enabled { "Disable" } else { "Enable" },
    ));
    body.push_str(&action_form(&record.id, token, "delete", "Delete"));
    body.push_str("</body></html>\n");
    body
}

fn live_bits(
    station: Option<&StationHandle>,
    record: &ChargerRecord,
) -> (bool, String, String, Vec<String>) {
    let Some(station) = station else {
        return (false, record.ocpp_version.clone(), "—".into(), Vec::new());
    };
    let owner = station
        .arbiter()
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .lock_owner()
        .unwrap_or("—")
        .to_string();
    (
        station.connected(),
        station.version_label(&record.ocpp_version),
        owner,
        station.backends().ids(),
    )
}

fn service_form(charger_id: &str, service: &ServiceRecord, token: &str) -> String {
    let title = if service.id.is_empty() {
        "Add service".to_string()
    } else {
        format!("Service {}", esc(&service.id))
    };
    let enabled = if service.enabled { "checked" } else { "" };
    let delete = if service.id.is_empty() {
        String::new()
    } else {
        format!(
            r#"<form method="post" action="/chargers/{id}/services/{sid}/delete"><input type="hidden" name="token" value="{token}"><button type="submit">Remove</button></form>"#,
            id = esc(charger_id),
            sid = esc(&service.id),
            token = esc(token),
        )
    };
    format!(
        r#"<h3>{title}</h3>
<form method="post" action="/chargers/{id}/services">
  <input type="hidden" name="token" value="{token}">
  <label>Id <input name="id" value="{sid}" required></label>
  <label>URL <input name="url" value="{url}"></label>
  <label>Version <input name="version" value="{version}"></label>
  <label>Auth <input name="auth_type" value="{auth}"></label>
  <label>Username <input name="username" value="{user}"></label>
  <label>Password <input name="password" value="{password}"></label>
  <label>Auth token <input name="auth_token" value="{auth_token}"></label>
  <label>Enabled <input type="checkbox" name="enabled" value="true" {enabled}></label>
  <button type="submit">Save</button>
</form>
{delete}
"#,
        id = esc(charger_id),
        token = esc(token),
        sid = esc(&service.id),
        url = esc(&service.url),
        version = esc(&service.version),
        auth = esc(&service.auth_type),
        user = esc(&service.username),
        password = esc(&service.password),
        auth_token = esc(&service.token),
        enabled = enabled,
    )
}

fn policy_form(record: &ChargerRecord, token: &str) -> String {
    format!(
        r#"<h2>Sharing policy</h2>
<form method="post" action="/chargers/{id}/policy">
  <input type="hidden" name="token" value="{token}">
  <label>OCPP version <input name="ocpp_version" value="{version}"></label>
  <label>Auto-detect <input type="checkbox" name="auto_detect" value="true" {auto}></label>
  <label>Allow shared charging <input type="checkbox" name="allow_shared_charging" value="true" {shared}></label>
  <label>Preferred provider <input name="preferred_provider" value="{preferred}"></label>
  <label>Blocked providers <input name="blocked_providers" value="{blocked}"></label>
  <label>Allowed providers <input name="allowed_providers" value="{allowed}"></label>
  <label>Presence sensor <input name="presence_sensor" value="{presence}"></label>
  <label>Override boolean <input name="override_input_boolean" value="{override_input}"></label>
  <label>Rate limit seconds <input name="rate_limit_seconds" value="{rate}"></label>
  <button type="submit">Save policy</button>
</form>
"#,
        id = esc(&record.id),
        token = esc(token),
        version = esc(&record.ocpp_version),
        auto = checked(record.auto_detect),
        shared = checked(record.allow_shared_charging),
        preferred = esc(&record.preferred_provider),
        blocked = esc(&record.blocked_providers.join(",")),
        allowed = esc(&record.allowed_providers.join(",")),
        presence = esc(&record.presence_sensor),
        override_input = esc(&record.override_input_boolean),
        rate = record.rate_limit_seconds,
    )
}

fn action_form(id: &str, token: &str, action: &str, label: &str) -> String {
    format!(
        r#"<form method="post" action="/chargers/{id}/{action}"><input type="hidden" name="token" value="{token}"><button type="submit">{label}</button></form>"#,
        id = esc(id),
        token = esc(token),
        action = esc(action),
        label = esc(label),
    )
}

fn checked(value: bool) -> &'static str {
    if value {
        "checked"
    } else {
        ""
    }
}

pub fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
