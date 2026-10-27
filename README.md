# LocalInferOS

A minimal BusyBox-based OS that boots straight into a terminal chat app for local LLMs (llama.cpp, CPU only). It profiles your hardware, estimates tok/s for each model, and downloads models from Hugging Face. Boots from a 16 GB USB stick (UEFI, Secure Boot off).

```
src/               Rust TUI (chat, profiling, downloads; starts llama-server)
vendor/llama.cpp/  Git submodule
iso/               build.sh (USB image), init (boot script), run-qemu.sh
```

## Build

```sh
git clone --recursive https://github.com/adixg/localinferos && cd localinferos
sudo apt install -y build-essential cmake ninja-build pkg-config libssl-dev busybox-static cpio \
  zstd grub-efi-amd64-bin mtools qemu-system-x86 ovmf

# llama.cpp, CPU only, portable to any AVX2 CPU
cmake -S vendor/llama.cpp -B build/llama-cpu -G Ninja -DCMAKE_BUILD_TYPE=Release -DGGML_NATIVE=OFF \
  -DGGML_CUDA=OFF -DLLAMA_BUILD_TESTS=OFF -DLLAMA_BUILD_EXAMPLES=OFF -DLLAMA_BUILD_SERVER=ON
cmake --build build/llama-cpu --target llama-server llama-bench -j 8

# Bundled default model
mkdir -p models && curl -L -o models/Qwen3-1.7B-Q4_K_M.gguf \
  https://huggingface.co/ggml-org/Qwen3-1.7B-GGUF/resolve/main/Qwen3-1.7B-Q4_K_M.gguf

iso/build.sh     # -> dist/LocalInferOS-v0.1.img, 14 GiB (asks for sudo once)
iso/run-qemu.sh  # boot it in QEMU as a USB stick; RAM=8G CPUS=8 to override
```

Flash the image with balenaEtcher, Rufus (DD mode) or `dd`. QEMU needs KVM access: `sudo usermod -aG kvm $USER`, then restart WSL (`wsl --shutdown`).

## Commands

| Command | |
|---|---|
| `/profile` | Benchmark this machine, estimate tok/s per model (runs on first boot) |
| `/download N` | Download model N from the profile table (`N force` if it may not fit in RAM) |
| `/cancel` | Stop the download (`/download` again resumes) |
| `/delete N`, `/storage` | Delete a downloaded model, show disk usage |
| `/models`, `/use N` | List models, switch model (clears chat) |
| `/clear`, `/help` | Reset chat, list commands |

↑/↓, PgUp/PgDn and End scroll; Ctrl+Q quits. `localinferos --profile` prints the profile without the TUI.
