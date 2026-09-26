//! prost-generated types re-export
#![allow(clippy::all)]

pub mod v1 {
    include!(concat!(env!("OUT_DIR"), "/neuroos.v1.rs"));
}
