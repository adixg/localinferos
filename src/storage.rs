// Disk usage of the data partition and of downloaded models.
//
// llama-server's router downloads into $LLAMA_CACHE (/data/hf-cache on the USB stick)
// using the Hugging Face cache layout: one models--<org>--<repo> folder per repo.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::profile::CatalogModel;

pub fn models_dir() -> PathBuf {
    PathBuf::from(std::env::var("LLAMA_CACHE").unwrap_or_else(|_| "/data/hf-cache".to_owned()))
}

/// (total, free) bytes on the filesystem that holds the models folder.
pub fn disk_space() -> Result<(u64, u64), String> {
    let dir = models_dir();
    // -P: one line per filesystem, in the same format for BusyBox and GNU df
    let output = Command::new("df")
        .args(["-P", "-k"])
        .arg(&dir)
        .output()
        .map_err(|err| format!("could not run df: {err}"))?;
    if !output.status.success() {
        return Err(format!("df failed for {}", dir.display()));
    }
    // Filesystem 1024-blocks Used Available Capacity Mounted-on
    let text = String::from_utf8_lossy(&output.stdout);
    let fields: Vec<u64> = text
        .lines()
        .nth(1)
        .ok_or("unexpected df output")?
        .split_whitespace()
        .skip(1)
        .take(3)
        .filter_map(|f| f.parse().ok())
        .collect();
    let [total, _used, free] = fields[..] else {
        return Err("unexpected df output".to_owned());
    };
    Ok((total * 1024, free * 1024))
}

/// Bytes on disk for a catalog model, including a partial download.
pub fn model_usage(model: &CatalogModel) -> u64 {
    dir_size(&models_dir().join(format!("models--{}", model.repo.replace('/', "--"))))
}

fn dir_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(t) if t.is_dir() => dir_size(&entry.path()),
            Ok(t) if t.is_file() => entry.metadata().map_or(0, |m| m.len()),
            // Symlinks (snapshots/ -> blobs/) would count the same file twice.
            _ => 0,
        })
        .sum()
}
