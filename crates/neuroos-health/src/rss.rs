//! Resident set size of the current process, from `/proc/self/status`.
use std::io;

pub fn rss_bytes() -> io::Result<u64> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest
                .trim()
                .strip_suffix(" kB")
                .unwrap_or(rest.trim())
                .trim()
                .parse()
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidData, "unparseable VmRSS line")
                })?;
            return Ok(kb * 1024);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "VmRSS not found in /proc/self/status",
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    #[test]
    fn rss_is_positive_for_the_running_process() {
        let rss = rss_bytes().unwrap();
        assert!(rss > 0);
    }
}
