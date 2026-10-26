use std::{
    error::Error,
    io::{BufRead, BufReader},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph, Wrap},
    DefaultTerminal, Frame,
};
use serde::Serialize;
use serde_json::{json, Value};

mod download;
mod profile;
mod storage;

const SERVER_URL: &str = "http://127.0.0.1:8080/v1/chat/completions";
const MODELS_URL: &str = "http://127.0.0.1:8080/v1/models";

// The model bundled on the ISO, at models/default/ (see iso/build.sh).
const DEFAULT_MODEL: &str = "default";
const DEFAULT_MODEL_FILE: &str = "Qwen3-1.7B-Q4_K_M";

fn display_name(model: &str) -> String {
    if model == DEFAULT_MODEL {
        format!("{model} ({DEFAULT_MODEL_FILE})")
    } else {
        model.to_owned()
    }
}


fn fetch_models() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let response: serde_json::Value =
        reqwest::blocking::get(MODELS_URL)?
            .error_for_status()?
            .json()?;

    let models = response["data"]
        .as_array()
        .ok_or("Missing models array")?
        .iter()
        // Downloads in progress are listed too, but can't be used yet.
        .filter(|m| !matches!(m["status"]["value"].as_str(), Some("downloading" | "incomplete")))
        .filter_map(|m| m["id"].as_str().map(str::to_owned))
        .collect();

    Ok(models)
}


#[derive(Clone, Serialize)]
struct Message {
    role: String,
    content: String,
}

enum WorkerEvent {
    Token(String),
    Done,
    Error(String),
    Profile(Result<profile::Profile, String>),
    DownloadProgress { model: String, done: u64, total: u64 },
    DownloadDone { model: String, error: Option<String> },
}

const STALL_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RETRIES: u32 = 5;

struct Download {
    name: &'static str,
    model: String,
    done: u64,
    total: u64,
    last_progress: Instant,
    retries: u32,
    /// Cancelling a stalled download to retry it reports a failure; skip those.
    ignore_failures: u32,
}


#[derive(Default)]
struct App {
    input: String,
    transcript: String,
    messages: Vec<Message>,
    response: String,
    busy: bool,
    scroll_up: u16,
    models: Vec<String>,
    selected_model: String,
    profiling: bool,
    profile: Option<profile::Profile>,
    download: Option<Download>,
}

impl App {
    fn new() -> Self {
        let mut app = App::default();
        app.refresh_models();
        app.selected_model = app.preferred_model();
        if app.selected_model.is_empty() {
            app.transcript.push_str("[No models found. Is llama-server running?]\n");
        }
        app.profile = profile::load();
        if let Some(p) = &app.profile {
            app.transcript.push_str(&p.report());
            app.transcript.push_str("[Saved profile. Run /profile to measure again.]\n");
        }
        app
    }

    fn start_profile(&mut self, tx: Sender<WorkerEvent>) {
        self.transcript.push_str("\n[Profiling this machine with llama-bench, about a minute...]\n");
        self.scroll_up = 0;
        self.busy = true;
        self.profiling = true;
        thread::spawn(move || {
            let _ = tx.send(WorkerEvent::Profile(profile::run()));
        });
    }

    fn start_download(&mut self, arg: &str, tx: Sender<WorkerEvent>) {
        if let Some(d) = &self.download {
            self.transcript.push_str(&format!("\n[Already downloading {}. /cancel to stop it]\n", d.name));
            return;
        }
        let (num, force) = match arg.strip_suffix(" force") {
            Some(num) => (num.trim(), true),
            None => (arg, false),
        };
        let Some(entry) = self.catalog_entry(num) else {
            return;
        };
        if let Some(p) = &self.profile {
            if !p.fits(entry) && !force {
                self.transcript.push_str(&format!(
                    "\n[{} probably won't fit in this machine's RAM. Use /download {num} force to download it anyway]\n",
                    entry.name
                ));
                return;
            }
        }
        // A resumed download only needs the part that isn't on disk yet.
        // If df fails, skip the check rather than block the download.
        if let Ok((_, free)) = storage::disk_space() {
            let needed = entry.size.saturating_sub(storage::model_usage(entry));
            if needed > free {
                self.transcript.push_str(&format!(
                    "\n[Not enough disk space for {}: needs {:.1} GB, {:.1} GB free. See /storage]\n",
                    entry.name,
                    needed as f64 / 1e9,
                    free as f64 / 1e9
                ));
                return;
            }
        }

        let model = entry.id();
        match download::status(&model).as_deref() {
            None | Some("incomplete") => {}
            Some("downloading") => {} // e.g. started before the TUI restarted
            Some(_) => {
                self.transcript.push_str(&format!("\n[{} is already downloaded]\n", entry.name));
                self.list_models();
                return;
            }
        }

        self.transcript.push_str(&format!(
            "\n[Downloading {} ({:.1} GB) from Hugging Face...]\n",
            entry.name,
            entry.size as f64 / 1e9
        ));
        self.scroll_up = 0;
        self.download = Some(Download {
            name: entry.name,
            model: model.clone(),
            done: 0,
            total: entry.size,
            last_progress: Instant::now(),
            retries: 0,
            ignore_failures: 0,
        });
        thread::spawn(move || {
            if let Err(err) = download::start(&model) {
                let _ = tx.send(WorkerEvent::DownloadDone { model, error: Some(err) });
            }
        });
    }

    /// Looks up a model by its number in the /profile table.
    fn catalog_entry(&mut self, num: &str) -> Option<&'static profile::CatalogModel> {
        let entry = num
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| profile::CATALOG.get(i));
        if entry.is_none() {
            self.transcript.push_str(&format!("\n[Unknown model number: {num}. See the table from /profile]\n"));
        }
        entry
    }

    fn delete_model(&mut self, num: &str) {
        let Some(entry) = self.catalog_entry(num) else {
            return;
        };
        let model = entry.id();
        if self.download.as_ref().is_some_and(|d| d.model == model) {
            self.transcript.push_str(&format!("\n[{} is downloading. /cancel it first]\n", entry.name));
            return;
        }
        let used = storage::model_usage(entry);
        if download::status(&model).is_none() && used == 0 {
            self.transcript.push_str(&format!("\n[{} isn't downloaded]\n", entry.name));
            return;
        }
        if let Err(err) = download::remove(&model) {
            self.transcript.push_str(&format!("\n[Could not delete {}: {err}]\n", entry.name));
            return;
        }
        self.transcript.push_str(&format!(
            "\n[Deleted {}, freed {:.1} GB]\n",
            entry.name,
            used as f64 / 1e9
        ));
        self.refresh_models();
        if self.selected_model == model {
            self.messages.clear();
            self.response.clear();
            self.selected_model = self.preferred_model();
            self.transcript.push_str(&format!("[Now using {}]\n", display_name(&self.selected_model)));
        }
    }

    fn show_storage(&mut self) {
        let mut out = String::from("\n");
        match storage::disk_space() {
            Ok((total, free)) => out.push_str(&format!(
                "Data disk: {:.1} GB used of {:.1} GB ({:.1} GB free)\n",
                (total - free) as f64 / 1e9,
                total as f64 / 1e9,
                free as f64 / 1e9
            )),
            Err(err) => out.push_str(&format!("[Could not read disk space: {err}]\n")),
        }
        let mut any = false;
        for (i, entry) in profile::CATALOG.iter().enumerate() {
            let used = storage::model_usage(entry);
            if used == 0 {
                continue;
            }
            any = true;
            let partial = if used < entry.size { "  (partial download)" } else { "" };
            out.push_str(&format!("{:>3}  {:<22} {:>5.1} GB{partial}\n", i + 1, entry.name, used as f64 / 1e9));
        }
        if any {
            out.push_str("Free space with /delete N\n");
        } else {
            out.push_str("No downloaded models. Get one with /download N\n");
        }
        self.transcript.push_str(&out);
        self.scroll_up = 0;
    }

    fn cancel_download(&mut self) {
        let Some(d) = self.download.take() else {
            self.transcript.push_str("\n[No download in progress]\n");
            return;
        };
        self.transcript.push_str(&format!("\n[Cancelled {}. /download it again to resume]\n", d.name));
        thread::spawn(move || {
            let _ = download::cancel(&d.model);
        });
    }

    /// Called every frame: restarts a download that has stopped making progress.
    fn check_download_stall(&mut self) {
        let Some(d) = &mut self.download else {
            return;
        };
        if d.last_progress.elapsed() < STALL_TIMEOUT {
            return;
        }
        if d.retries >= MAX_RETRIES {
            self.transcript.push_str(&format!(
                "\n[{} keeps stalling. Try /download again later; it resumes where it stopped]\n",
                d.name
            ));
            let model = d.model.clone();
            self.download = None;
            thread::spawn(move || {
                let _ = download::cancel(&model);
            });
            return;
        }
        d.retries += 1;
        d.ignore_failures += 1;
        d.last_progress = Instant::now();
        self.transcript.push_str(&format!("\n[Download stalled, retrying ({}/{MAX_RETRIES})...]\n", d.retries));
        let model = d.model.clone();
        thread::spawn(move || {
            let _ = download::cancel(&model);
            thread::sleep(Duration::from_secs(1));
            let _ = download::start(&model);
        });
    }

    /// The bundled model if the router lists it, otherwise the first model.
    fn preferred_model(&self) -> String {
        self.models
            .iter()
            .find(|m| *m == DEFAULT_MODEL)
            .or_else(|| self.models.first())
            .cloned()
            .unwrap_or_default()
    }

    fn refresh_models(&mut self) {
        match fetch_models() {
            Ok(models) => self.models = models,
            Err(err) => self.transcript.push_str(&format!("\n[ERROR: {err}]\n")),
        }
    }

    fn list_models(&mut self) {
        self.refresh_models();
        self.transcript.push_str("\nModels:\n");
        for (i, model) in self.models.iter().enumerate() {
            let mark = if *model == self.selected_model { "*" } else { " " };
            self.transcript.push_str(&format!("{mark} {}. {}\n", i + 1, display_name(model)));
        }
    }

    fn use_model(&mut self, arg: &str) {
        let model = arg
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| self.models.get(i))
            .or_else(|| self.models.iter().find(|m| *m == arg))
            .cloned();
        let Some(model) = model else {
            self.transcript.push_str(&format!("\n[Unknown model: {arg}. Try /models]\n"));
            return;
        };
        if model != self.selected_model {
            self.messages.clear();
            self.response.clear();
            self.selected_model = model;
        }
        self.transcript.push_str(&format!("\n[Using {}]\n", display_name(&self.selected_model)));
    }

    fn submit(&mut self, tx: Sender<WorkerEvent>) {
        if self.busy {
            return;
        }
        let prompt = self.input.trim().to_owned();
        self.input.clear();
        if prompt.is_empty() {
            return;
        }
        if prompt == "/clear" {
            self.messages.clear();
            self.response.clear();
            self.transcript.clear();
            self.scroll_up = 0;
            return;
        }
        if prompt == "/models" {
            self.list_models();
            return;
        }
        if let Some(arg) = prompt.strip_prefix("/use ") {
            self.use_model(arg.trim());
            return;
        }
        if let Some(arg) = prompt.strip_prefix("/download ") {
            self.start_download(arg.trim(), tx);
            return;
        }
        if prompt == "/cancel" {
            self.cancel_download();
            return;
        }
        if let Some(arg) = prompt.strip_prefix("/delete ") {
            self.delete_model(arg.trim());
            return;
        }
        if prompt == "/storage" {
            self.show_storage();
            return;
        }
        if prompt == "/profile" {
            self.start_profile(tx);
            return;
        }
        if prompt == "/help" {
            self.transcript.push_str("\n/profile  benchmark this machine, estimate tok/s per model\n/download N  download model N from the /profile table (N force: even if it may not fit)\n/cancel  stop the current download\n/delete N  delete downloaded model N\n/storage  show disk usage\n/models  list models\n/use N | /use <id>  switch model (clears context)\n/clear  reset chat\n/help  this help\n");
            return;
        }
        if self.download.is_some() {
            // Loading a model for chat can evict the download (--models-max 1).
            self.transcript.push_str("\n[Chat is paused while a model downloads. /cancel to stop it]\n");
            return;
        }
        if self.selected_model.is_empty() {
            self.transcript.push_str("\n[No model selected. Try /models]\n");
            return;
        }

        self.transcript.push_str(&format!("\nYOU: {prompt}\nASSISTANT: "));
        self.messages.push(Message {
            role: "user".to_owned(),
            content: prompt,
        });
        self.response.clear();
        self.scroll_up = 0;
        self.busy = true;

        let messages = self.messages.clone();
        let model = self.selected_model.clone();
        thread::spawn(move || {
            if let Err(err) = stream_chat(&model, messages, &tx) {
                let _ = tx.send(WorkerEvent::Error(err.to_string()));
            }
        });
    }

    fn on_worker_event(&mut self, event: WorkerEvent) {
        match event {
            WorkerEvent::Token(text) => {
                self.transcript.push_str(&text);
                self.response.push_str(&text);
            }
            WorkerEvent::Done => {
                if !self.response.is_empty() {
                    self.messages.push(Message {
                        role: "assistant".to_owned(),
                        content: std::mem::take(&mut self.response),
                    });
                } else {
                    self.transcript.push_str("[No response text returned]");
                }
                self.transcript.push('\n');
                self.busy = false;
            }
            WorkerEvent::Error(err) => {
                self.transcript.push_str(&format!("\n[ERROR: {err}]\n"));
                // Keep failed requests out of the model's context.
                if self.messages.last().is_some_and(|m| m.role == "user") {
                    self.messages.pop();
                }
                self.response.clear();
                self.busy = false;
            }
            WorkerEvent::Profile(result) => {
                match result {
                    Ok(p) => {
                        self.transcript.push_str(&p.report());
                        if let Err(err) = profile::save(&p) {
                            self.transcript.push_str(&format!("[Could not save profile: {err}]\n"));
                        }
                        self.profile = Some(p);
                    }
                    Err(err) => self.transcript.push_str(&format!("[Profiling failed: {err}]\n")),
                }
                self.busy = false;
                self.profiling = false;
            }
            WorkerEvent::DownloadProgress { model, done, total } => {
                if let Some(d) = self.download.as_mut().filter(|d| d.model == model) {
                    if done > d.done {
                        d.last_progress = Instant::now();
                    }
                    d.done = done;
                    if total > 0 {
                        d.total = total;
                    }
                }
            }
            WorkerEvent::DownloadDone { model, error } => {
                let Some(d) = self.download.as_mut().filter(|d| d.model == model) else {
                    return;
                };
                match error {
                    Some(_) if d.ignore_failures > 0 => d.ignore_failures -= 1,
                    Some(err) => {
                        self.transcript.push_str(&format!(
                            "\n[Downloading {} failed: {err}. /download it again to resume]\n",
                            d.name
                        ));
                        self.download = None;
                    }
                    None => {
                        self.transcript.push_str(&format!("\n[Downloaded {}. Switch to it with /use]\n", d.name));
                        self.download = None;
                        self.list_models();
                    }
                }
            }
        }
    }
}


fn stream_chat(model: &str, messages: Vec<Message>, tx: &Sender<WorkerEvent>) -> Result<(), Box<dyn Error + Send + Sync>> {
    // No overall timeout: long replies, or loading a model on first use, can
    // take minutes. reqwest's blocking client otherwise stops at 30 seconds.
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(None::<Duration>)
        .build()?;

    let response = client
        .post(SERVER_URL)
        .json(&json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "max_tokens": 512,
            "chat_template_kwargs": { "enable_thinking": false }
        }))
        .send()?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text()?;
            return Err(format!("HTTP {status}: {body}").into());
        }

    // OpenAI-compatible streaming responses use Server-Sent Events (SSE).
    for line in BufReader::new(response).lines() {
        let line = line?;
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        let chunk: Value = serde_json::from_str(data)?;
        if let Some(message) = chunk.get("error") {
            return Err(format!("Server error: {message}").into());
        }
        if let Some(text) = chunk["choices"][0]["delta"]["content"].as_str() {
            if !text.is_empty() && tx.send(WorkerEvent::Token(text.to_owned())).is_err() {
                return Ok(()); // UI has exited.
            }
        }
    }
    let _ = tx.send(WorkerEvent::Done);
    Ok(())
}

fn draw(frame: &mut Frame, app: &mut App) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(frame.area());

    let title = Paragraph::new(format!("LocalInferOS  |  {}", display_name(&app.selected_model)))
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(title, sections[0]);

    let chat = Paragraph::new(app.transcript.as_str())
        .wrap(Wrap { trim: false })
        .block(Block::default().title(" Chat ").borders(Borders::ALL));
    let available_height = sections[1].height.saturating_sub(2);
    let rendered_lines = chat.line_count(sections[1].width.max(1));
    let bottom = rendered_lines.saturating_sub(available_height as usize);
    app.scroll_up = app.scroll_up.min(bottom.min(u16::MAX as usize) as u16);
    let scroll = (bottom - app.scroll_up as usize).min(u16::MAX as usize) as u16;
    frame.render_widget(chat.scroll((scroll, 0)), sections[1]);

    let prompt = Paragraph::new(format!("> {}_", app.input))
        .block(Block::default().title(" Message ").borders(Borders::ALL));
    frame.render_widget(prompt, sections[2]);

    let status = if app.profiling {
        "Profiling...".to_owned()
    } else if let Some(d) = &app.download {
        format!(
            "Downloading {} {:.0}% ({:.1}/{:.1} GB)",
            d.name,
            100.0 * d.done as f64 / d.total.max(1) as f64,
            d.done as f64 / 1e9,
            d.total as f64 / 1e9
        )
    } else if app.busy {
        "Generating...".to_owned()
    } else {
        "Ready".to_owned()
    };
    let help = format!("{status}  |  Enter send  |  /help commands  |  ↑/↓ PgUp/PgDn scroll  |  Ctrl+Q quit");
    frame.render_widget(Paragraph::new(help), sections[3]);
}

fn run(terminal: &mut DefaultTerminal) -> std::io::Result<()> {
    let mut app = App::new();
    let (tx, rx): (Sender<WorkerEvent>, Receiver<WorkerEvent>) = mpsc::channel();
    if app.profile.is_none() {
        app.start_profile(tx.clone());
    }
    download::watch(tx.clone());

    loop {
        while let Ok(message) = rx.try_recv() {
            app.on_worker_event(message);
        }
        app.check_download_stall();
        terminal.draw(|frame| draw(frame, &mut app))?;

        if event::poll(Duration::from_millis(40))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match (key.code, key.modifiers) {
                    (KeyCode::Char('q'), m) if m.contains(KeyModifiers::CONTROL) => break,
                    (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => break,
                    (KeyCode::Enter, _) => app.submit(tx.clone()),
                    (KeyCode::Backspace, _) => { app.input.pop(); }
                    (KeyCode::PageUp, _) => {
                        app.scroll_up = app.scroll_up.saturating_add(10);
                    }
                    (KeyCode::PageDown, _) => {
                        app.scroll_up = app.scroll_up.saturating_sub(10);
                    }
                    (KeyCode::Up, _) => app.scroll_up = app.scroll_up.saturating_add(1),
                    (KeyCode::Down, _) => app.scroll_up = app.scroll_up.saturating_sub(1),
                    (KeyCode::End, _) => app.scroll_up = 0,
                    (KeyCode::Char(c), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                        if !app.busy {
                            app.input.push(c);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn main() -> std::io::Result<()> {
    // `localinferos --profile` prints the hardware report without the TUI.
    if std::env::args().nth(1).as_deref() == Some("--profile") {
        match profile::run() {
            Ok(p) => {
                print!("{}", p.report());
                if let Err(err) = profile::save(&p) {
                    eprintln!("Could not save profile: {err}");
                }
            }
            Err(err) => eprintln!("Profiling failed: {err}"),
        }
        return Ok(());
    }
    ratatui::run(run)
}
