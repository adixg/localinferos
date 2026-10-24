# LocalInferOS — minimal Rust prototype

This is the original working Ratatui TUI with **one small addition**: switch between GGUF models via llama.cpp's router. No new crates, persistent database, browser UI, or Python runtime.

## 1. Start the model router

From `~/projects/localos/llama.cpp`:

```sh
./build-cuda/bin/llama-server \
  --models-dir "$HOME/projects/localos/models" \
  --models-max 1 \
  --no-webui \
  -ngl 99 -c 4096 \
  --host 127.0.0.1 --port 8080
```

Put `.gguf` files in `~/projects/localos/models/`. `--models-max 1` prevents multiple models from being kept loaded at the same time. The router may create a child inference process for the active model.

Confirm the router reports models:

```sh
curl -s http://127.0.0.1:8080/v1/models | jq -r '.data[].id'
```

If the request fails, check `./build-cuda/bin/llama-server --help` for router support; rebuild an up-to-date llama.cpp if needed.

## 2. Run the TUI

```sh
cd ~/projects/localos/localinferos-minimal
cargo run --release
```

The TUI automatically queries `/v1/models` on startup and chooses the first discovered model.

Commands typed into the existing chat input:

- `/models` — refresh and list discovered models
- `/use 1` — choose model number 1, clears in-memory context if switching
- `/use <exact-model-id>` — alternatively choose using the model id
- `/clear` — reset chat
- `/help` — show commands
- Ctrl+Q — quit

No model downloads, login, or chat persistence in this increment. These can be added later without changing the ISO design.

## Minimality

`reqwest` TLS features are disabled: the TUI only talks to HTTP on `127.0.0.1`. Future Hugging Face downloads will require enabling TLS in the downloader (or reusing llama.cpp's own downloader). This is a feature choice, not a promise of a single statically linked executable.

The ISO and Linux distribution are *not* built by this package. The app and API work on WSL; the final ISO will still need a target-compatible binary and NVIDIA drivers.
