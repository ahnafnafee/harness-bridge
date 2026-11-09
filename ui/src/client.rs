use serde_json::Value;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

pub fn storage_get(name: &str) -> Option<String> {
    web_sys::window()?
        .session_storage()
        .ok()??
        .get_item(name)
        .ok()?
}
pub fn storage_set(name: &str, value: &str) {
    if let Some(storage) = web_sys::window().and_then(|w| w.session_storage().ok().flatten()) {
        let _ = storage.set_item(name, value);
    }
}
pub fn access_key() -> Result<String, String> {
    let window = web_sys::window().ok_or("Browser window is unavailable")?;
    let hash = window.location().hash().unwrap_or_default();
    if let Some(key) = hash
        .strip_prefix("#key=")
        .filter(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()))
    {
        storage_set("bridge-key", key);
        if let Ok(history) = window.history() {
            let _ = history.replace_state_with_url(&JsValue::NULL, "", Some("/"));
        }
        return Ok(key.into());
    }
    storage_get("bridge-key").ok_or("Open the complete local URL printed by harness-bridge".into())
}
fn error(value: JsValue) -> String {
    value.as_string().unwrap_or_else(|| format!("{value:?}"))
}
pub async fn request(path: &str, body: Option<Value>, key: &str) -> Result<Value, String> {
    let options = web_sys::RequestInit::new();
    options.set_method(if body.is_some() { "POST" } else { "GET" });
    if let Some(body) = body {
        options.set_body(&JsValue::from_str(&body.to_string()));
    }
    let request = web_sys::Request::new_with_str_and_init(path, &options).map_err(error)?;
    request
        .headers()
        .set("X-Harness-Bridge-Key", key)
        .map_err(error)?;
    if options.get_method().as_deref() == Some("POST") {
        request
            .headers()
            .set("Content-Type", "application/json")
            .map_err(error)?;
    }
    let window = web_sys::window().ok_or("Browser window is unavailable")?;
    let response: web_sys::Response = JsFuture::from(window.fetch_with_request(&request))
        .await
        .map_err(error)?
        .dyn_into()
        .map_err(error)?;
    let text = JsFuture::from(response.text().map_err(error)?)
        .await
        .map_err(error)?
        .as_string()
        .unwrap_or_default();
    let json: Value = serde_json::from_str(&text)
        .map_err(|_| "The local service returned an invalid response".to_string())?;
    if !response.ok() {
        return Err(json["error"]
            .as_str()
            .unwrap_or("The operation failed")
            .into());
    }
    Ok(json)
}

pub fn copy(text: String) {
    if let Some(window) = web_sys::window() {
        let promise = window.navigator().clipboard().write_text(&text);
        wasm_bindgen_futures::spawn_local(async move {
            let _ = JsFuture::from(promise).await;
        });
    }
}
