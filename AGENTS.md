# AGENTS.md

OCPP proxy workspace. Dependencies point inward.

| Crate | Layer | May name |
|---|---|---|
| `crates/domain` (`ocpp-proxy-domain`) | domain | config values, version detection, the control lock, session math. No Tokio, Axum, or `rust-ocpp`. |
| `crates/application` (`ocpp-proxy-application`) | application | use cases and port traits. No HTTP types and no driver crates. |
| `crates/ocpp-proxy` | adapters and composition root | Axum, SQLite, Home Assistant, the OCPP-J codec. |

Only `crates/ocpp-proxy/src/main.rs` and `spawn.rs` construct concrete adapters. Tests bind an ephemeral port through `spawn`.
