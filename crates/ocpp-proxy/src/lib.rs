//! OCPP proxy process. Concrete adapters are constructed in [`spawn`].

mod backend;
mod charger;
mod clock;
mod codec;
mod ha;
mod html;
mod link;
mod payload;
mod peer;
mod rest;
mod spawn;
mod state;
mod store;
mod upstream;

pub use clock::SystemClock;
pub use codec::{decode, encode, Frame};
pub use ha::{websocket_url, HaClient};
pub use spawn::{spawn, ProxyOptions, RunningProxy};
pub use store::{column_names, table_exists, SqliteStore};
