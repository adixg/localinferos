// Hardware profiling and model recommendations.
//
// On a CPU, token generation is limited by memory bandwidth: every token reads
// (roughly) all of the model's weights once. So we benchmark one model, derive
// the machine's effective bandwidth, and estimate other models from their size.

use std::{fs, path::PathBuf, process::Command};

use serde::{Deserialize, Serialize};
use serde_json::Value;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

pub struct CatalogModel {
    pub name: &'static str,
    pub repo: &'static str,
    /// Quantization tag; llama-server downloads `repo:quant`.
    pub quant: &'static str,
    /// GGUF file size in bytes. Every token reads (roughly) all of it.
    pub size: u64,
}

impl CatalogModel {
    /// The model id the llama-server router uses for a downloaded model.
    pub fn id(&self) -> String {
        format!("{}:{}", self.repo, self.quant)
    }
}

const fn model(name: &'static str, repo: &'static str, quant: &'static str, size: u64) -> CatalogModel {
    CatalogModel { name, repo, quant, size }
}

pub const CATALOG: &[CatalogModel] = &[
    model("Qwen3 0.6B (Q8_0)", "Qwen/Qwen3-0.6B-GGUF", "Q8_0", 639_446_688),
    model("Gemma 3 1B", "ggml-org/gemma-3-1b-it-GGUF", "Q4_K_M", 806_058_240),
    model("Llama 3.2 1B", "bartowski/Llama-3.2-1B-Instruct-GGUF", "Q4_K_M", 807_694_464),
    model("Qwen3 1.7B", "ggml-org/Qwen3-1.7B-GGUF", "Q4_K_M", 1_282_439_264),
    model("Llama 3.2 3B", "bartowski/Llama-3.2-3B-Instruct-GGUF", "Q4_K_M", 2_019_377_696),
    model("Gemma 3 4B", "ggml-org/gemma-3-4b-it-GGUF", "Q4_K_M", 2_489_757_856),
    model("Qwen3 4B", "Qwen/Qwen3-4B-GGUF", "Q4_K_M", 2_497_280_256),
    model("Llama 3.1 8B", "bartowski/Meta-Llama-3.1-8B-Instruct-GGUF", "Q4_K_M", 4_920_739_232),
    model("Qwen3 8B", "Qwen/Qwen3-8B-GGUF", "Q4_K_M", 5_027_783_488),
    model("Gemma 3 12B", "ggml-org/gemma-3-12b-it-GGUF", "Q4_K_M", 7_300_574_976),
    model("Mistral Nemo 12B", "bartowski/Mistral-Nemo-Instruct-2407-GGUF", "Q4_K_M", 7_477_208_192),
    model("Qwen3 14B", "Qwen/Qwen3-14B-GGUF", "Q4_K_M", 9_001_752_960),
    model("Phi-4 14B", "bartowski/phi-4-GGUF", "Q4_K_M", 9_053_114_816),
    model("Mistral Small 3.2 24B", "bartowski/mistralai_Mistral-Small-3.2-24B-Instruct-2506-GGUF", "Q4_K_M", 14_333_915_264),
    model("Gemma 3 27B", "ggml-org/gemma-3-27b-it-GGUF", "Q4_K_M", 16_546_404_736),
];

#[derive(Serialize, Deserialize)]
pub struct Profile {
    pub cpu: String,
    pub cpu_threads: usize,
    pub ram_bytes: u64,
    pub best_threads: usize,
    pub tokens_per_sec: f64,
    /// Effective memory bandwidth in bytes/s, measured through llama-bench.
    pub bandwidth: f64,
}

impl Profile {
    /// Within about ±15% of measured speeds for Qwen3 0.6B/4B/8B on an
    /// i7-13650HX. The 1.7B benchmark model uses bandwidth slightly better
    /// than other models, hence the 0.85 factor.
    pub fn estimate(&self, model: &CatalogModel) -> f64 {
        0.85 * self.bandwidth / model.size as f64
    }

    /// Weights plus room for the KV cache, plus 1 GiB for everything else.
    pub fn fits(&self, model: &CatalogModel) -> bool {
        model.size as f64 * 1.2 + GIB <= self.ram_bytes as f64
    }

    pub fn report(&self) -> String {
        let mut out = format!(
            "\nHardware: {} | {} threads | {:.1} GiB RAM\n\
             Measured: {:.1} tok/s with {} threads (~{:.0} GB/s effective)\n\n\
             {:>3}  {:<22} {:>8} {:>10}  {}\n",
            self.cpu,
            self.cpu_threads,
            self.ram_bytes as f64 / GIB,
            self.tokens_per_sec,
            self.best_threads,
            self.bandwidth / 1e9,
            "#",
            "Model",
            "Size",
            "Est tok/s",
            "Fits RAM",
        );
        for (i, model) in CATALOG.iter().enumerate() {
            out.push_str(&format!(
                "{:>3}  {:<22} {:>5.1} GB {:>10.0}  {}\n",
                i + 1,
                model.name,
                model.size as f64 / 1e9,
                self.estimate(model),
                if self.fits(model) { "yes" } else { "no" },
            ));
        }
        out.push_str("\nDownload one with /download N\n");
        out
    }
}

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_owned())
}

fn profile_path() -> PathBuf {
    PathBuf::from(env_or("LOCALINFEROS_DATA", "/data")).join("config/profile.json")
}

pub fn load() -> Option<Profile> {
    serde_json::from_str(&fs::read_to_string(profile_path()).ok()?).ok()
}

pub fn save(profile: &Profile) -> std::io::Result<()> {
    let path = profile_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, serde_json::to_string_pretty(profile)?)
}

fn cpu_name() -> String {
    fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|info| {
            info.lines()
                .find(|l| l.starts_with("model name"))
                .and_then(|l| l.split_once(':'))
                .map(|(_, name)| name.trim().to_owned())
        })
        .unwrap_or_else(|| "unknown CPU".to_owned())
}

fn ram_bytes() -> u64 {
    fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|info| {
            info.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<u64>().ok())
        })
        .unwrap_or(0)
        * 1024
}

/// Thread counts to try: generation stops scaling once memory is saturated,
/// usually well below the total thread count.
fn thread_candidates(threads: usize) -> Vec<usize> {
    let mut list = vec![2, 4, 6, 8, threads / 2, threads];
    list.retain(|&t| t >= 1 && t <= threads);
    list.sort_unstable();
    list.dedup();
    if list.is_empty() {
        list.push(1);
    }
    list
}

/// One llama-bench run at `threads`. Returns (tokens/s, model size in bytes).
fn bench_once(bench: &str, model: &str, threads: usize) -> Result<(f64, f64), String> {
    let output = Command::new(bench)
        .args(["-m", model, "-ngl", "0", "-p", "0", "-n", "64", "-r", "1"])
        .args(["-t", &threads.to_string(), "-o", "json"])
        .output()
        .map_err(|err| format!("could not run {bench}: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(5).collect();
        return Err(format!("llama-bench failed: {}", tail.into_iter().rev().collect::<Vec<_>>().join(" | ")));
    }

    let results: Vec<Value> = serde_json::from_slice(&output.stdout)
        .map_err(|err| format!("could not parse llama-bench output: {err}"))?;
    let result = results.first().ok_or("llama-bench returned no results")?;
    let tokens_per_sec = result["avg_ts"].as_f64().ok_or("missing avg_ts")?;
    let model_size = result["model_size"].as_f64().ok_or("missing model_size")?;
    Ok((tokens_per_sec, model_size))
}

/// Runs llama-bench on the bundled model with increasing thread counts,
/// stopping at the first count that is slower than the best so far.
pub fn run() -> Result<Profile, String> {
    let bench = env_or("LLAMA_BENCH", "llama-bench");
    let model = env_or("LOCALINFEROS_PROFILE_MODEL", "/data/models/default/Qwen3-1.7B-Q4_K_M.gguf");
    let cpu_threads = std::thread::available_parallelism().map_or(1, |n| n.get());

    let (mut best_threads, mut tokens_per_sec, mut model_size) = (0, 0.0, 0.0);
    for threads in thread_candidates(cpu_threads) {
        let (ts, size) = bench_once(&bench, &model, threads)?;
        if ts < tokens_per_sec {
            break;
        }
        (best_threads, tokens_per_sec, model_size) = (threads, ts, size);
    }
    if tokens_per_sec <= 0.0 {
        return Err("llama-bench reported no speed".to_owned());
    }
    // Single runs occasionally report impossible speeds, too high or too low
    // (likely clock jumps, e.g. under WSL). Confirm the winner; if the two runs
    // disagree, a third run breaks the tie.
    let (confirm, _) = bench_once(&bench, &model, best_threads)?;
    if (confirm - tokens_per_sec).abs() <= 0.2 * tokens_per_sec.max(confirm) {
        tokens_per_sec = (tokens_per_sec + confirm) / 2.0;
    } else {
        let (third, _) = bench_once(&bench, &model, best_threads)?;
        let mut runs = [tokens_per_sec, confirm, third];
        runs.sort_by(f64::total_cmp);
        tokens_per_sec = runs[1];
    }

    Ok(Profile {
        cpu: cpu_name(),
        cpu_threads,
        ram_bytes: ram_bytes(),
        best_threads,
        tokens_per_sec,
        bandwidth: tokens_per_sec * model_size,
    })
}
