//! C3 storage core: ingest filter, domain adapters, embedding, LanceDB/SQLite,
//! query APIs, lifecycle jobs. Split from `main.rs` so tests can exercise
//! real logic without a second process (mirrors `neuroos-healthd`/
//! `neuroos-monitor`'s own lib/main split).
pub mod adapters;
pub mod embed;
pub mod engine;
pub mod ingest;
pub mod lance;
pub mod lifecycle;
pub mod server;
pub mod spool;
pub mod sqlite;
