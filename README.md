# LocalInferOS

A minimal Ratatui TUI for chatting with local GGUF models through llama.cpp's router, with switching between models. No persistent database, browser UI, or Python runtime.

Busybox base OS that you can boot into using a usb stick. Currently meant for 16GB USB sticks.

## Repo structure

```
LocalInferOS/
├── Cargo.toml
├── Cargo.lock
├── src/                 # Rust TUI
├── vendor/
│   └── llama.cpp/       # Git submodule
├── iso/
│   └── build.sh
├── .gitmodules
├── .gitignore
└── README.md
```

## 0. Clone

```sh
git clone --recursive https://github.com/adixg/localinferos
```

If you cloned without `--recursive`, run `git submodule update --init`.

## 1. Start the model router

Build llama.cpp in `vendor/llama.cpp` (e.g. with CUDA into `build-cuda/`), then from the repo root:

```sh
vendor/llama.cpp/build-cuda/bin/llama-server \
  --models-dir ./models \
  --models-max 1 \
  --no-webui \
  -ngl 99 -c 4096 \
  --host 127.0.0.1 --port 8080
```

Put `.gguf` files in `./models/` (ignored by git). `--models-max 1` prevents multiple models from being kept loaded at the same time. The router may create a child inference process for the active model.

Confirm the router reports models:

```sh
curl -s http://127.0.0.1:8080/v1/models | jq -r '.data[].id'
```

If the request fails, check `vendor/llama.cpp/build-cuda/bin/llama-server --help` for router support; update the submodule and rebuild if needed.

## 2. Run the TUI

From the repo root:

```sh
cargo run --release
```

The TUI automatically queries `/v1/models` on startup and chooses the first discovered model.

Commands typed into the chat input:

- `/profile` — benchmark this machine and estimate tok/s for each catalog model (runs automatically on first start; saved to `/data/config/profile.json`)
- `/models` — refresh and list discovered models
- `/use 1` — choose model number 1, clears in-memory context if switching
- `/use <exact-model-id>` — alternatively choose using the model id
- `/clear` — reset chat
- `/help` — show commands
- ↑/↓ scroll one line, PgUp/PgDn scroll 10 lines, End jump to latest
- Ctrl+Q — quit

No model downloads, login, or chat persistence yet. These can be added later without changing the ISO design.

## Updating llama.cpp

```sh
cd vendor/llama.cpp && git pull && cd ../..
git add vendor/llama.cpp && git commit -m "Update llama.cpp"
```

## Minimality

`reqwest` TLS features are disabled: the TUI only talks to HTTP on `127.0.0.1`. Future Hugging Face downloads will require enabling TLS in the downloader (or reusing llama.cpp's own downloader). This is a feature choice, not a promise of a single statically linked executable.

The ISO is not built yet (`iso/build.sh` is a placeholder). The app and API work on WSL; the final ISO will still need a target-compatible binary and NVIDIA drivers.
