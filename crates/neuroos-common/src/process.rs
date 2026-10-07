//! Process identity.
//!
//! C1: the real uid used to be fetched through a hand-rolled
//! `unsafe extern "C" { fn getuid() -> u32; }` block copy-pasted into twelve
//! crates, which violated rules.md §6 (`unsafe` belongs only in
//! `neuroos-shm`, `neuroos-sandbox` and FFI shims). `rustix` wraps the same
//! syscall safely, so there is now exactly one caller and no `unsafe`
//! anywhere outside the two crates that are allowed it.

/// The real uid of this process.
///
/// Used for the `SO_PEERCRED` allowlists on every Unix socket in the system
/// (Architecture.md §5.2) and for the `/run/user/<uid>` runtime directory.
/// `getuid()` cannot fail.
pub fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[test]
    fn current_uid_matches_the_real_process_uid() {
        // `id -u` is the independent oracle: comparing against another
        // getuid() call would only prove the function calls itself.
        let out = std::process::Command::new("id")
            .arg("-u")
            .output()
            .expect("`id -u` must be available");
        let expected: u32 = String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse()
            .expect("`id -u` prints a number");
        assert_eq!(current_uid(), expected);
    }
}
