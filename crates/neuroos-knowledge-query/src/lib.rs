//! C5a knowledge-query core: deictic snap, evidence retrieval, prompt
//! assembly, taint wrapping, graph view render. Split from `main.rs` so
//! tests can exercise real logic without a second process (mirrors
//! `neuroos-healthd`/`neuroos-monitor`/`neuroos-storage`'s own lib/main
//! split).
pub mod assemble;
pub mod deictic;
pub mod distill;
pub mod evidence;
pub mod graph_view;
pub mod inference_client;
pub mod kernel_client;
pub mod orchestrate;
pub mod server;
pub mod storage_client;
pub mod taint_wrap;
pub mod voice_client;

/// H15 / Architecture.md §8.2's C5a row: its own sockets in the runtime
/// dir and the one `graph_view.html` it renders (D3 is compiled in). Every
/// other component is reached over a socket, which Landlock does not
/// restrict; no stored user data is readable directly.
pub fn sandbox_policy() -> neuroos_sandbox::Policy {
    neuroos_sandbox::Policy::baseline()
        .read_write(neuroos_common::paths::runtime_dir())
        .read_write(neuroos_common::paths::graph_view_html_file())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[test]
    fn sandbox_policy_matches_the_c5a_row() {
        let policy = sandbox_policy();
        assert!(policy.allows_write_to(&neuroos_common::paths::graph_view_html_file()));
        assert!(policy.allows_write_to(&neuroos_common::paths::knowledge_sock()));
        assert!(!policy.allows_write_to(&neuroos_common::paths::data_dir().join("other.html")));
        assert!(!policy.allows_write_to(&neuroos_common::paths::storage_dir()));
    }
}
