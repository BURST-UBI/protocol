#[cfg(target_arch = "wasm32")]
pub(crate) use burst_browser::{sleep, timeout};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use tokio::time::{sleep, timeout};
