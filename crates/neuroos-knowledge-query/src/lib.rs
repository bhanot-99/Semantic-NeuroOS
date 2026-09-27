//! C5a knowledge-query core: deictic snap, evidence retrieval, prompt
//! assembly, taint wrapping, graph view render. Split from `main.rs` so
//! tests can exercise real logic without a second process (mirrors
//! `neuroos-healthd`/`neuroos-monitor`/`neuroos-storage`'s own lib/main
//! split).
pub mod assemble;
pub mod deictic;
pub mod evidence;
pub mod graph_view;
pub mod storage_client;
pub mod taint_wrap;
