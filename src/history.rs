// Chat history on the data partition: one JSON file per conversation in
// /data/history, named by the time it started, so the newest sorts last.

use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{storage, Message};

#[derive(Serialize, Deserialize)]
pub struct Chat {
    pub model: String,
    pub messages: Vec<Message>,
}

fn dir() -> PathBuf {
    storage::data_dir().join("history")
}

/// An id for a new conversation: its start time in seconds.
pub fn new_id() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

pub fn save(id: u64, chat: &Chat) -> std::io::Result<()> {
    fs::create_dir_all(dir())?;
    fs::write(dir().join(format!("{id}.json")), serde_json::to_string_pretty(chat)?)
}

/// The most recent conversation, if any.
pub fn load_latest() -> Option<(u64, Chat)> {
    let id = fs::read_dir(dir())
        .ok()?
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.strip_suffix(".json")?.parse::<u64>().ok())
        .max()?;
    let chat = serde_json::from_str(&fs::read_to_string(dir().join(format!("{id}.json"))).ok()?).ok()?;
    Some((id, chat))
}
