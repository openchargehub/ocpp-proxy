//! Use cases. Adapters implement the traits; this crate does not open sockets.

mod control;
mod events;
mod messages;
mod ports;
mod registry;
mod sessions;

pub use control::{
    acquire_control, remote_start, remote_stop, RemoteStart, RemoteStop, StartDecision,
    StopDecision,
};
pub use events::ChargerEvent;
pub use messages::{fault_message_v16, fault_message_v201, is_fault_status, session_ended_message};
pub use ports::{
    BackendHub, ChargerCommand, Clock, EntityState, Notifier, SessionStore, UpstreamHub,
};
pub use registry::{
    ChargerDefaults, ChargerRecord, ChargerRegistry, EnrollOutcome, ServiceRecord,
};
pub use sessions::{
    list_sessions, note_fault, notify_session, on_boot, on_heartbeat, on_meter, on_start,
    on_start_v201, on_status, on_transaction_stop, AcceptedCall, StartInput, StopInput,
};
