// Model downloads through llama-server's router, which fetches GGUF files from
// Hugging Face into $LLAMA_CACHE (/data/hf-cache on the USB stick). Once a download
// finishes, the router lists the model and loads it on first use.

use std::{
    error::Error,
    io::{BufRead, BufReader},
    sync::mpsc::Sender,
    thread,
    time::Duration,
};

use serde_json::{json, Value};

use crate::WorkerEvent;

const ROUTER_URL: &str = "http://127.0.0.1:8080";

fn post(path: &str, body: Value) -> Result<(), String> {
    let response = reqwest::blocking::Client::new()
        .post(format!("{ROUTER_URL}{path}"))
        .json(&body)
        .send()
        .map_err(|err| err.to_string())?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().unwrap_or_default();
        return Err(format!("HTTP {status}: {text}"));
    }
    Ok(())
}

/// Starts a download, or resumes a cancelled one. Skips vision files
/// (`sidecars`), since the TUI only does text.
pub fn start(model: &str) -> Result<(), String> {
    post("/models", json!({ "model": model, "sidecars": [] }))
}

/// Cancels a running download. Its partial file is kept, so `start` resumes it.
pub fn cancel(model: &str) -> Result<(), String> {
    post("/models/unload", json!({ "model": model }))
}

/// Deletes a downloaded model's files. The router unloads it first if it's
/// running, and stops listing it.
pub fn remove(model: &str) -> Result<(), String> {
    let response = reqwest::blocking::Client::new()
        .delete(format!("{ROUTER_URL}/models?model={}", url_encode(model)))
        .send()
        .map_err(|err| err.to_string())?;
    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().unwrap_or_default();
        return Err(format!("HTTP {status}: {text}"));
    }
    Ok(())
}

/// Percent-encodes everything but unreserved characters, e.g. the `/` and `:`
/// in "Qwen/Qwen3-4B-GGUF:Q4_K_M".
fn url_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The router's status for `model` ("downloading", "incomplete", "unloaded",
/// "loaded", ...), or None if it doesn't know the model.
pub fn status(model: &str) -> Option<String> {
    let response: Value = reqwest::blocking::get(format!("{ROUTER_URL}/v1/models")).ok()?.json().ok()?;
    response["data"]
        .as_array()?
        .iter()
        .find(|m| m["id"] == model)
        .and_then(|m| m["status"]["value"].as_str())
        .map(str::to_owned)
}

/// Forwards the router's download events to the UI for the life of the app,
/// reconnecting whenever the router restarts or isn't up yet.
pub fn watch(tx: Sender<WorkerEvent>) {
    thread::spawn(move || loop {
        let _ = watch_once(&tx);
        thread::sleep(Duration::from_secs(2));
    });
}

fn watch_once(tx: &Sender<WorkerEvent>) -> Result<(), Box<dyn Error>> {
    // The event stream stays open indefinitely, so no overall timeout.
    let client = reqwest::blocking::Client::builder().timeout(None::<Duration>).build()?;
    let response = client.get(format!("{ROUTER_URL}/models/sse")).send()?.error_for_status()?;

    for line in BufReader::new(response).lines() {
        let line = line?;
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let Ok(event) = serde_json::from_str::<Value>(data.trim()) else {
            continue;
        };
        let model = event["model"].as_str().unwrap_or_default().to_owned();
        let message = match event["event"].as_str() {
            Some("download_progress") => {
                // One entry per file being downloaded, keyed by URL.
                let (mut done, mut total) = (0, 0);
                if let Some(files) = event["data"]["progress"].as_object() {
                    for file in files.values() {
                        done += file["done"].as_u64().unwrap_or(0);
                        total += file["total"].as_u64().unwrap_or(0);
                    }
                }
                WorkerEvent::DownloadProgress { model, done, total }
            }
            Some("download_finished") => WorkerEvent::DownloadDone { model, error: None },
            Some("download_failed") => WorkerEvent::DownloadDone { model, error: Some("download failed".to_owned()) },
            _ => continue,
        };
        let _ = tx.send(message);
    }
    Ok(())
}
