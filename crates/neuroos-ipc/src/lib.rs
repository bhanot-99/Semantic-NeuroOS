//! framing, UDS server/client, SO_PEERCRED, deadlines, reconnect
// C1 / rules.md §6: `unsafe` is allowed only in neuroos-shm,
// neuroos-sandbox and FFI shims. This enforces that.
#![cfg_attr(not(test), deny(unsafe_code))]

pub mod client;
pub mod deadline;
pub mod fd_framing;
pub mod framing;
pub mod peercred;
pub mod request;
pub mod server;

pub use client::{Backoff, ReconnectPolicy, connect, connect_retrying, connect_with_reconnect};
pub use deadline::{read_envelope_deadline, write_envelope_deadline};
pub use fd_framing::{
    read_envelope_with_fd, read_envelope_with_fd_deadline, write_envelope_with_fd,
};
pub use framing::{
    DEFAULT_MAX_FRAME, FramingError, read_envelope, read_frame, write_envelope, write_frame,
};
pub use peercred::{PeerCred, is_allowed, peer_cred};
pub use request::{OnRestart, RequestError, request_once};
pub use server::{
    ConnectionPermit, DEFAULT_IDLE_TIMEOUT, DEFAULT_MAX_CONNECTIONS, UdsServer, UdsServerConfig,
};
