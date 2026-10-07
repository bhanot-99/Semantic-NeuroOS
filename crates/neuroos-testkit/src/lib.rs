// C1 / rules.md §6: `unsafe` is allowed only in neuroos-shm,
// neuroos-sandbox and FFI shims. This enforces that.
#![deny(unsafe_code)]

// mock servers, fixture loaders, replay tooling
pub mod health_mocks;
pub mod inference_mocks;
pub mod kernel_mocks;
pub mod storage_mocks;
pub mod voice_mocks;
