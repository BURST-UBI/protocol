#[cfg(target_arch = "wasm32")]
pub(crate) use burst_web_runtime::{sleep, timeout};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use tokio::time::{sleep, timeout};
