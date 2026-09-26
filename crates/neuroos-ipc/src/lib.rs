//! framing, UDS server/client, SO_PEERCRED, deadlines, reconnect
pub mod client;
pub mod deadline;
pub mod framing;
pub mod peercred;
pub mod server;

pub use client::{ReconnectPolicy, connect, connect_with_reconnect};
pub use deadline::{read_envelope_deadline, write_envelope_deadline};
pub use framing::{
    DEFAULT_MAX_FRAME, FramingError, read_envelope, read_frame, write_envelope, write_frame,
};
pub use peercred::{PeerCred, is_allowed, peer_cred};
pub use server::{UdsServer, UdsServerConfig};
