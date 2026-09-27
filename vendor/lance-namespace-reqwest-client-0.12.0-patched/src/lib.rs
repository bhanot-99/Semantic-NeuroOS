#![allow(unused_imports)]
#![allow(clippy::too_many_arguments)]

extern crate serde_repr;
extern crate serde;
extern crate serde_json;
extern crate url;
// PATCHED (rules.md R0-1): reqwest and `apis` (the generated HTTP client
// that uses it) are now gated behind the "client" feature, off by
// default. `models` never referenced reqwest.
#[cfg(feature = "client")]
extern crate reqwest;

#[cfg(feature = "client")]
pub mod apis;
pub mod models;
