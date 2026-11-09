//! Optional, offline browser interface. Only loopback is accepted; a per-run
//! capability protects every data endpoint, including reads. Plans are single
//! use, expire, and are discarded when source settings change.

use crate::{
    app::{self, TransferPlan, TransferRequest},
    ir::SessionRef,
    providers::Homes,
};
use anyhow::Context;
use serde::{de::DeserializeOwned, Deserialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io::Read,
    net::{Ipv4Addr, SocketAddrV4},
    time::{Duration, Instant},
};
use tiny_http::{Header, Method, Request, Response, Server};

const MAX_BODY: u64 = 65_536;
const PLAN_TTL: Duration = Duration::from_secs(600);
const HTML: &str = include_str!("web/gpui.html");
const BOOT_JS: &str = include_str!("web/boot.js");
const HOST_CSS: &str = include_str!("web/host.css");
const GPUI_JS: &str = include_str!("web/gpui/harness_bridge_ui.js");
const GPUI_WASM: &[u8] = include_bytes!("web/gpui/harness_bridge_ui_bg.wasm.gz");
const ICON: &[u8] = include_bytes!("../assets/icon.png");
const WINDOWS_ICON: &[u8] = include_bytes!("../assets/icon.ico");
const SANS_FONT: &[u8] = include_bytes!("../assets/fonts/ibm-plex-sans.woff2");
const MONO_FONT: &[u8] = include_bytes!("../assets/fonts/ibm-plex-mono.woff2");

fn random_id() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|e| anyhow::anyhow!("cannot create local interface key: {e}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

struct UiState {
    homes: Homes,
    plans: HashMap<String, (Instant, TransferPlan)>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    provider: String,
    id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteRequest {
    plan_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportRequest {
    provider: String,
    id: String,
    path: std::path::PathBuf,
    #[serde(default)]
    include_subagents: bool,
}

fn read_json<T: DeserializeOwned>(request: &mut Request) -> anyhow::Result<T> {
    anyhow::ensure!(
        header(request, "Content-Type")
            .is_some_and(|value| value.split(';').next() == Some("application/json")),
        "expected application/json"
    );
    anyhow::ensure!(
        request
            .body_length()
            .is_none_or(|len| len as u64 <= MAX_BODY),
        "request is too large"
    );
    let mut bytes = Vec::new();
    request
        .as_reader()
        .take(MAX_BODY + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() as u64 <= MAX_BODY, "request is too large");
    serde_json::from_slice(&bytes).context("invalid request")
}

impl UiState {
    fn reference(&self, selection: &Selection) -> anyhow::Result<SessionRef> {
        app::discover_provider(&selection.provider, &self.homes)?
            .into_iter()
            .find(|r| r.id == selection.id)
            .context("the selected session is no longer available; refresh the list")
    }

    fn api(&mut self, request: &mut Request) -> anyhow::Result<Value> {
        self.plans.retain(|_, (time, _)| time.elapsed() < PLAN_TTL);
        match (request.method(), request.url()) {
            (&Method::Get, "/api/catalog") => {
                let catalog = app::discover(&self.homes);
                Ok(
                    json!({"catalog":catalog,"providers":crate::providers::CATALOG,"settings":self.homes,"version":env!("CARGO_PKG_VERSION")}),
                )
            }
            (&Method::Post, "/api/inspect") => {
                let selection: Selection = read_json(request)?;
                Ok(serde_json::to_value(app::inspect(
                    &self.reference(&selection)?,
                    &self.homes,
                )?)?)
            }
            (&Method::Post, "/api/preview") => {
                let transfer: TransferRequest = read_json(request)?;
                let plan = TransferPlan::prepare(transfer, &self.homes)?;
                if self.plans.len() >= 8 {
                    self.plans.clear();
                }
                let id = random_id()?;
                let report = plan.report();
                self.plans.insert(id.clone(), (Instant::now(), plan));
                Ok(json!({"plan_id":id,"preview":report}))
            }
            (&Method::Post, "/api/write") => {
                let write: WriteRequest = read_json(request)?;
                let (_, plan) = self
                    .plans
                    .remove(&write.plan_id)
                    .context("preview expired or has already been used; prepare a new preview")?;
                Ok(serde_json::to_value(plan.execute()?)?)
            }
            (&Method::Post, "/api/export") => {
                let export: ExportRequest = read_json(request)?;
                let reference = self.reference(&Selection {
                    provider: export.provider,
                    id: export.id,
                })?;
                app::export(
                    &reference,
                    &self.homes,
                    &export.path,
                    export.include_subagents,
                    false,
                )
            }
            (&Method::Post, "/api/settings") => {
                let homes: Homes = read_json(request)?;
                self.homes = homes;
                self.plans.clear();
                Ok(json!({"saved":true}))
            }
            _ => anyhow::bail!("unknown operation"),
        }
    }
}

fn header<'a>(request: &'a Request, name: &'static str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv(name))
        .map(|header| header.value.as_str())
}

fn allowed(
    host: Option<&str>,
    origin: Option<&str>,
    fetch_site: Option<&str>,
    expected_host: &str,
) -> bool {
    host == Some(expected_host)
        && origin.is_none_or(|origin| origin == format!("http://{expected_host}"))
        && fetch_site.is_none_or(|site| matches!(site, "same-origin" | "none"))
}

fn respond(request: Request, status: u16, content_type: &str, body: impl Into<Vec<u8>>) {
    respond_encoded(request, status, content_type, body, false);
}

fn respond_encoded(
    request: Request,
    status: u16,
    content_type: &str,
    body: impl Into<Vec<u8>>,
    gzip: bool,
) {
    let mut response = Response::from_data(body.into()).with_status_code(status);
    for (name,value) in [
        ("Content-Type",content_type),("Cache-Control","no-store"),("X-Content-Type-Options","nosniff"),
        ("Referrer-Policy","no-referrer"),("Cross-Origin-Resource-Policy","same-origin"),
        ("Content-Security-Policy","default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data: blob:; font-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'")
    ] {response.add_header(Header::from_bytes(name,value).expect("static response header"));}
    if gzip {
        response.add_header(
            Header::from_bytes("Content-Encoding", "gzip").expect("static response header"),
        );
    }
    let _ = request.respond(response);
}

pub fn serve(homes: Homes, port: u16, open: bool) -> anyhow::Result<()> {
    let server = Server::http(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
        .map_err(|e| anyhow::anyhow!("cannot start local interface: {e}"))?;
    let address = server
        .server_addr()
        .to_ip()
        .context("expected loopback listener")?;
    let expected_host = address.to_string();
    let key = random_id()?;
    let url = format!("http://{address}/#key={key}");
    println!("harness-bridge local interface\n\n{url}\n\nKeep this process running. Press Ctrl+C to stop.");
    if open {
        open_browser(&url)?;
    }
    let mut state = UiState {
        homes,
        plans: HashMap::new(),
    };
    for mut request in server.incoming_requests() {
        if !allowed(
            header(&request, "Host"),
            header(&request, "Origin"),
            header(&request, "Sec-Fetch-Site"),
            &expected_host,
        ) {
            respond(
                request,
                403,
                "application/json",
                br#"{"error":"request must come from this local interface"}"#.to_vec(),
            );
            continue;
        }
        if request.url().starts_with("/api/") {
            if header(&request, "X-Harness-Bridge-Key") != Some(key.as_str()) {
                respond(
                    request,
                    401,
                    "application/json",
                    br#"{"error":"open the full local URL printed by harness-bridge"}"#.to_vec(),
                );
                continue;
            }
            match state.api(&mut request) {
                Ok(value) => respond(
                    request,
                    200,
                    "application/json; charset=utf-8",
                    value.to_string(),
                ),
                Err(error) => respond(
                    request,
                    400,
                    "application/json; charset=utf-8",
                    json!({"error":format!("{error:#}")}).to_string(),
                ),
            }
        } else if request.method() == &Method::Get {
            match request.url() {
                "/" => respond(request, 200, "text/html; charset=utf-8", HTML),
                "/boot.js" => respond(request, 200, "text/javascript; charset=utf-8", BOOT_JS),
                "/host.css" => respond(request, 200, "text/css; charset=utf-8", HOST_CSS),
                "/gpui/harness_bridge_ui.js" => {
                    respond(request, 200, "text/javascript; charset=utf-8", GPUI_JS)
                }
                "/gpui/harness_bridge_ui_bg.wasm" => {
                    respond_encoded(request, 200, "application/wasm", GPUI_WASM, true)
                }
                "/icon.png" => respond(request, 200, "image/png", ICON),
                "/favicon.ico" => respond(request, 200, "image/x-icon", WINDOWS_ICON),
                "/fonts/ibm-plex-sans.woff2" => respond(request, 200, "font/woff2", SANS_FONT),
                "/fonts/ibm-plex-mono.woff2" => respond(request, 200, "font/woff2", MONO_FONT),
                _ => respond(request, 404, "text/plain", "Not found"),
            }
        } else {
            respond(request, 405, "text/plain", "Method not allowed");
        }
    }
    Ok(())
}

fn open_browser(url: &str) -> anyhow::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        std::process::Command::new("rundll32.exe")
            .args(["url.dll,FileProtocolHandler", url])
            .creation_flags(0x08000000)
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn bundled_gpui_matches_sources_and_generated_assets() {
        let manifest: Value = serde_json::from_str(include_str!("web/gpui/build.json")).unwrap();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        for (path, expected) in manifest["sources"].as_object().unwrap() {
            let bytes = std::fs::read(root.join(path)).unwrap();
            let bytes = if path.ends_with(".ttf") {
                bytes
            } else {
                String::from_utf8(bytes)
                    .unwrap()
                    .replace("\r\n", "\n")
                    .into_bytes()
            };
            assert_eq!(
                format!("{:x}", Sha256::digest(bytes)),
                expected.as_str().unwrap(),
                "rebuild the GPUI frontend after changing {path}"
            );
        }
        for (name, bytes) in [
            ("harness_bridge_ui.js", GPUI_JS.as_bytes()),
            ("harness_bridge_ui_bg.wasm.gz", GPUI_WASM),
        ] {
            assert_eq!(
                format!("{:x}", Sha256::digest(bytes)),
                manifest["assets"][name].as_str().unwrap(),
                "GPUI asset checksum mismatch: {name}"
            );
        }
        assert_eq!(manifest["gpui_kit"], "0.9.0");
    }

    #[test]
    fn local_interface_rejects_foreign_origins_and_rebinding() {
        let host = "127.0.0.1:43210";
        assert!(allowed(
            Some(host),
            Some("http://127.0.0.1:43210"),
            Some("same-origin"),
            host
        ));
        assert!(allowed(Some(host), None, Some("none"), host));
        assert!(!allowed(Some("evil.example:43210"), None, None, host));
        assert!(!allowed(
            Some(host),
            Some("https://evil.example"),
            None,
            host
        ));
        assert!(!allowed(Some(host), None, Some("cross-site"), host));
    }
}
