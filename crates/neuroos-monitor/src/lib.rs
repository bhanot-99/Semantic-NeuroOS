//! C1 monitor core: sensors, event bus, privacy layer, record/replay dump
//! format. Split from `main.rs` so integration tests can drive real sensor
//! and bus logic without needing a second process (mirrors
//! `neuroos-healthd`'s split).
pub mod bus;
pub mod dump;
pub mod privacy;
pub mod sensors;

pub fn current_uid() -> u32 {
    // SAFETY: getuid() takes no arguments and cannot fail.
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}
