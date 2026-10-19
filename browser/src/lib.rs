//! Browser transport shared by the wallet and off-chain group client.
#![cfg(target_arch = "wasm32")]

use std::{future::Future, time::Duration};
use wasm_bindgen::{prelude::wasm_bindgen, JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = setTimeout)]
    fn set_timeout(callback: &js_sys::Function, delay: i32) -> JsValue;
    #[wasm_bindgen(js_name = clearTimeout)]
    fn clear_timeout(handle: &JsValue);
    #[wasm_bindgen(catch, js_name = fetch)]
    fn fetch(request: &web_sys::Request) -> Result<js_sys::Promise, JsValue>;
}

struct Timer(JsValue);
impl Drop for Timer {
    fn drop(&mut self) {
        clear_timeout(&self.0);
    }
}

/// Uses the browser event loop, including inside a Web Worker. Dropping the
/// future clears the scheduled callback instead of leaking a pending timer.
pub async fn sleep(duration: Duration) {
    let mut remaining = duration.as_millis();
    loop {
        let delay = remaining.min(i32::MAX as u128) as i32;
        let mut timer = None;
        let promise = js_sys::Promise::new(&mut |resolve, _| {
            timer = Some(Timer(set_timeout(&resolve, delay)));
        });
        let _timer = timer;
        let _ = JsFuture::from(promise).await;
        remaining -= delay as u128;
        if remaining == 0 {
            break;
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("operation timed out")]
pub struct TimeoutError;

pub async fn timeout<F: Future>(duration: Duration, future: F) -> Result<F::Output, TimeoutError> {
    use futures_util::future::{select, Either};
    match select(Box::pin(future), Box::pin(sleep(duration))).await {
        Either::Left((value, _)) => Ok(value),
        Either::Right(_) => Err(TimeoutError),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("request timed out")]
    Timeout,
    #[error("HTTP {0}")]
    Http(u16),
    #[error("browser request failed: {0}")]
    Transport(String),
    #[error("response exceeds {0} bytes")]
    TooLarge(usize),
    #[error("invalid browser response")]
    Invalid,
}

fn transport(value: JsValue) -> FetchError {
    FetchError::Transport(value.as_string().unwrap_or_else(|| format!("{value:?}")))
}

struct Abort(web_sys::AbortController);
impl Drop for Abort {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// GET with no body, POST with a JSON body. Redirects fail before another host
/// receives wallet queries. CORS remains enabled; cookies and cached replies
/// are never used. Timeout and cancellation also abort an in-progress body.
pub async fn fetch_bytes(
    url: &str,
    json: Option<&str>,
    duration: Duration,
    limit: usize,
) -> Result<Vec<u8>, FetchError> {
    let abort = Abort(web_sys::AbortController::new().map_err(transport)?);
    let init = web_sys::RequestInit::new();
    init.set_method(if json.is_some() { "POST" } else { "GET" });
    init.set_credentials(web_sys::RequestCredentials::Omit);
    init.set_redirect(web_sys::RequestRedirect::Error);
    init.set_mode(web_sys::RequestMode::Cors);
    init.set_cache(web_sys::RequestCache::NoStore);
    init.set_signal(Some(&abort.0.signal()));
    if let Some(json) = json {
        init.set_body(&JsValue::from_str(json));
        let headers = web_sys::Headers::new().map_err(transport)?;
        headers
            .set("Content-Type", "application/json")
            .map_err(transport)?;
        init.set_headers(&headers);
    }
    let request = web_sys::Request::new_with_str_and_init(url, &init).map_err(transport)?;
    timeout(duration, async {
        let response = JsFuture::from(fetch(&request).map_err(transport)?)
            .await
            .map_err(transport)?
            .dyn_into::<web_sys::Response>()
            .map_err(|_| FetchError::Invalid)?;
        if !response.ok() {
            return Err(FetchError::Http(response.status()));
        }
        if response
            .headers()
            .get("Content-Length")
            .map_err(transport)?
            .and_then(|length| length.parse::<u64>().ok())
            .is_some_and(|length| length > limit as u64)
        {
            return Err(FetchError::TooLarge(limit));
        }
        let Some(stream) = response.body() else {
            return Ok(Vec::new());
        };
        let reader = stream
            .get_reader()
            .dyn_into::<web_sys::ReadableStreamDefaultReader>()
            .map_err(|_| FetchError::Invalid)?;
        let mut bytes = Vec::new();
        loop {
            let part = JsFuture::from(reader.read()).await.map_err(transport)?;
            let done = js_sys::Reflect::get(&part, &JsValue::from_str("done"))
                .map_err(transport)?
                .as_bool()
                .ok_or(FetchError::Invalid)?;
            if done {
                break;
            }
            let chunk = js_sys::Reflect::get(&part, &JsValue::from_str("value"))
                .map_err(transport)?
                .dyn_into::<js_sys::Uint8Array>()
                .map_err(|_| FetchError::Invalid)?;
            if chunk.length() as usize > limit - bytes.len() {
                return Err(FetchError::TooLarge(limit));
            }
            bytes.extend_from_slice(&chunk.to_vec());
        }
        reader.release_lock();
        Ok(bytes)
    })
    .await
    .map_err(|_| FetchError::Timeout)?
}
