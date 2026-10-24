use std::{
    error::Error,
    io::{BufRead, BufReader},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Duration,
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

const SERVER_URL: &str = "http://127.0.0.1:8080/v1/chat/completions";
const MODELS_URL: &str = "http://127.0.0.1:8080/v1/models";


fn fetch_models() -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let response: serde_json::Value =
        reqwest::blocking::get(MODELS_URL)?
            .error_for_status()?
            .json()?;

    let models = response["data"]
        .as_array()
        .ok_or("Missing models array")?
        .iter()
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
}

impl App {
    fn new() -> Self {
        let mut app = App::default();
        app.refresh_models();
        app.selected_model = app.models.first().cloned().unwrap_or_default();
        if app.selected_model.is_empty() {
            app.transcript.push_str("[No models found. Is llama-server running?]\n");
        }
        app
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
            self.transcript.push_str(&format!("{mark} {}. {model}\n", i + 1));
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
        self.transcript.push_str(&format!("\n[Using {}]\n", self.selected_model));
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
        if prompt == "/help" {
            self.transcript.push_str("\n/models  list models\n/use N | /use <id>  switch model (clears context)\n/clear  reset chat\n/help  this help\n");
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
        }
    }
}


fn stream_chat(model: &str, messages: Vec<Message>, tx: &Sender<WorkerEvent>) -> Result<(), Box<dyn Error + Send + Sync>> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(5))
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

    let title = Paragraph::new(format!("LocalInferOS  |  {}", app.selected_model))
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

    let status = if app.busy { "Generating..." } else { "Ready" };
    let help = format!("{status}  |  Enter send  |  /help commands  |  ↑/↓ PgUp/PgDn scroll  |  Ctrl+Q quit");
    frame.render_widget(Paragraph::new(help), sections[3]);
}

fn run(terminal: &mut DefaultTerminal) -> std::io::Result<()> {
    let mut app = App::new();
    let (tx, rx): (Sender<WorkerEvent>, Receiver<WorkerEvent>) = mpsc::channel();

    loop {
        while let Ok(message) = rx.try_recv() {
            app.on_worker_event(message);
        }
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
    ratatui::run(run)
}
