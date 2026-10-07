//! prost-generated types re-export
// C1 / rules.md §6: `unsafe` is allowed only in neuroos-shm,
// neuroos-sandbox and FFI shims. This enforces that.
#![deny(unsafe_code)]
#![allow(clippy::all)]

pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/neuroos.v1.rs"));
}
