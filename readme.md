# course2md

Turn YouTube, Bilibili, or local course/meeting recordings into slide-illustrated Markdown and HTML lecture notes.

[![Rust](https://img.shields.io/badge/Language-Rust-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey.svg)](#installation)
[![AUR](https://img.shields.io/aur/version/course2md-bin?color=blue)](https://aur.archlinux.org/packages/course2md-bin)

**English** · [中文](readme.zh.md)

---

## Quick Start

Simply provide an online video URL or a path to a local video file:

```bash
# Process a Bilibili video
course2md https://www.bilibili.com/video/BV1pb8o6yE8f

# Process a YouTube video
course2md https://youtu.be/dQw4w9WgXcQ

# Process a local lecture or meeting recording
course2md ./lecture.mp4
```

> **First Run Note**:
> - **macOS (Apple Silicon)**: Prebuilt binaries default to the CoreML backend. On the first run, model weights (~1–2 GB) are automatically fetched from HuggingFace to `~/Library/Caches/qwen3-speech/`. No manual setup required.
> - **Linux / Windows**: Defaults to the GPU/CPU backend via `llama.cpp`. On the first run, the GGUF ASR model (~2.4 GB) is automatically downloaded to `~/.cache/course2md/models/`.

---

## Installation

`course2md` relies on the following multimedia tools:
- `ffmpeg` & `ffprobe` (Audio/video extraction and slide sampling)
- `yt-dlp` (Online video parsing and downloading; only needed for online URLs)
- `llama-server` (Provided by `llama.cpp`; only needed for `gpu` / `cpu` backends, not required for macOS CoreML mode)

---

### macOS

Recommended installation via Homebrew:

```bash
# 1. Install dependencies (ffmpeg is sufficient if using CoreML with local files)
brew install ffmpeg yt-dlp llama.cpp

# 2. Install course2md (automatically installs binary and required mlx.metallib to ~/bin)
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash
```

---

### Arch Linux / CachyOS

Available on the **AUR** with automated dependency resolution:

```bash
# Install via AUR helper (first-class citizen)
yay -S course2md-bin
# or using paru:
# paru -S course2md-bin
```

<details>
<summary>Manual installation</summary>

```bash
# 1. Install dependencies
sudo pacman -S ffmpeg yt-dlp llama-cpp

# 2. Install course2md
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash
```
</details>

---

### Debian / Ubuntu

```bash
# 1. Install base dependencies and build tools
sudo apt update
sudo apt install -y ffmpeg yt-dlp git cmake build-essential

# 2. Build and install llama-server
git clone https://github.com/ggml-org/llama.cpp.git
cmake -S llama.cpp -B llama.cpp/build -DLLAMA_CURL=OFF
cmake --build llama.cpp/build --config Release -j
sudo install -m755 llama.cpp/build/bin/llama-server /usr/local/bin/llama-server

# 3. Install course2md
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash
```

---

### Windows

Install dependencies via `winget` in **PowerShell**:

```powershell
winget install --id Gyan.FFmpeg -e
winget install --id yt-dlp.yt-dlp -e
winget install --id ggml.llamacpp -e
```

> Alternatively, install via Scoop (`scoop install ffmpeg yt-dlp`) or Chocolatey. Ensure `ffmpeg`, `ffprobe`, `yt-dlp`, and `llama-server.exe` are in your `PATH`.

**Install course2md**:
1. Download `course2md-windows-x86_64.exe` from [Releases](https://github.com/mizorewww/course2md/releases).
2. Rename to `course2md.exe` and place it in a directory listed in your `PATH`.

---

### Building from Source

Requires the stable Rust toolchain:

```bash
git clone https://github.com/mizorewww/course2md.git
cd course2md

# Standard install
cargo install --path .

# Or build release binary only
cargo build --release
```

- **macOS Apple Silicon Note**: Building native CoreML support requires Xcode 16+ (Swift 6 toolchain). `build.rs` compiles the Swift package and copies `mlx.metallib` to the target directory. If you do not need native CoreML support, skip it via: `COURSE2MD_NO_APPLE=1 cargo build --release`.
- **Other Platforms**: Linux, Windows, and x86_64 macOS builds automatically skip Apple-native components.

---

## ASR Backends

`course2md` supports three speech recognition backends via `--provider <backend>` or configuration:

| Backend (`--provider`) | Target & Default Policy | Architecture & Models | External Dependencies | Model Download & Cache Path | Highlights |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **`coreml`** | **macOS Apple Silicon**<br>(Default for prebuilt arm64) | **Silero VAD v6.2.1 CoreML** (ANE)<br>+ **Qwen3-ASR 0.6B CoreML** ([speech-swift](https://github.com/soniqo/speech-swift)) | **Zero external dependencies**<br>(requires co-located `mlx.metallib`) | ~1–2 GB<br>`~/Library/Caches/qwen3-speech/`<br>*(supports `HF_ENDPOINT` mirror)* | Leverages Apple Neural Engine (ANE) and Metal; lightweight memory footprint; no daemon process |
| **`gpu`** | **Linux / Windows / Intel Mac**<br>(Default on non-Apple-Silicon) | **ffmpeg silencedetect**<br>+ **Qwen3-ASR 1.7B GGUF Q8** | Requires `llama-server`<br>(from `llama.cpp`) | ~2.4 GB<br>`~/.cache/course2md/models/` | High-precision 1.7B Q8 quantized model; accelerates via Metal / CUDA / Vulkan |
| **`cpu`** | **Universal Fallback** | Same as `gpu`, with `-ngl 0` | Requires `llama-server` | ~2.4 GB<br>`~/.cache/course2md/models/` | Pure CPU execution; maximum hardware compatibility |

> **Automatic Fallback**: On macOS, if the `coreml` backend fails during initialization or runtime, `course2md` automatically logs a warning and falls back to the `gpu` / `llama-server` pipeline to ensure task completion.

---

## Configuration

To avoid passing repetitive command-line arguments, `course2md` provides a global TOML configuration file.

### Configuration Path
- **macOS / Linux**: `~/.config/course2md/config.toml` (follows `$XDG_CONFIG_HOME`)
- **Windows**: `%APPDATA%\course2md\config.toml`

### Priority Hierarchy
**CLI Flags > Configuration File (config.toml) > Built-in Defaults**

### Configuration Management Commands

```bash
# 1. Generate an annotated configuration template (use --force to overwrite existing)
course2md config init

# 2. Display the configuration path and effective default settings
course2md config show
```

### Configuration File Structure

```toml
# ~/.config/course2md/config.toml

[defaults]
# Output root directory (structured as <out>/<platform>/<title>/<id>/)
out = "out"

# Frame similarity SSIM threshold (0.0 to 1.0; lower value = more slides captured)
similarity = 0.85

# Frame sampling check interval in seconds
sample_interval = 1.0

# Cooldown time (seconds) after a new slide is captured before capturing again
cooldown = 10.0

# Region of Interest (ROI), e.g. "40%,0%-100%,100%"; empty compares full frame
# roi = "40%,0%-100%,100%"

# ASR transcription thread count
threads = 4

# Inference backend: coreml (macOS Apple Silicon) | gpu | cpu
# provider = "coreml"

# Maximum speech segment duration in seconds before splitting
max_speech = 20.0

# Output document formats: md, html, json
formats = ["md", "html"]

# llama.cpp GGUF model directory (leave commented for default cache)
# model_dir = "~/.cache/course2md/models"

# Keep downloaded media.mp4 video file after processing
keep_video = false

[llm]
# Enable LLM subtitle polishing by default (default: false; run `course2md llm setup` to configure)
enabled = false

# OpenAI-compatible API endpoint (auto-prefixes https:// if omitted)
base_url = "https://api.deepseek.com/v1"

# API Key (file permissions automatically restricted to 0600 on Unix)
api_key = "sk-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"

# Model identifier
model = "deepseek-chat"

# Custom prompt (leave empty to use high-quality built-in proofreading prompt)
prompt = ""

# Permanently suppress the post-run LLM suggestion hint (default: false)
disable_hint = false
```

---

## LLM Subtitle Polishing (Optional)

`course2md` can automatically invoke a Large Language Model (LLM) after ASR transcription to proofread and refine the generated transcript.

- **Polishing Scope**: Corrects verbal tics and filler words (e.g., "um", "uh", "you know"), stuttering/repetitions, homophone typos, and technical terminology spelling. **Preserves original meaning, does not summarize, add, or translate content**.
- **Compatible Endpoints**: Any OpenAI-compatible `/chat/completions` API (e.g., DeepSeek, GLM, OpenAI, Ollama, vLLM).
- **Fault Tolerance**: Batches requests in 20-segment chunks (`temperature=0`). If a batch fails or returns invalid JSON, it automatically falls back to raw ASR text and logs a warning without halting the conversion.

### Management Commands

```bash
# Interactive setup and enablement (press Enter to keep existing values; tests connectivity upon save)
course2md llm setup

# Non-interactive configuration via flags
course2md llm setup --base-url https://api.deepseek.com/v1 --api-key sk-xxxx --model deepseek-chat

# View current LLM status (API Key masked)
course2md llm status

# Disable LLM polishing while preserving configured credentials
course2md llm disable
```

### CLI Overrides at Runtime

```bash
# Force enable / disable LLM polishing for a single run
course2md https://... --llm
course2md https://... --no-llm

# Temporarily override endpoint, key, or model
course2md https://... --llm --llm-base-url https://api.deepseek.com/v1 --llm-api-key sk-xxxx --llm-model deepseek-chat

# Suppress post-run LLM suggestion hint for a single run
course2md https://... --no-llm-hint
```

---

## Output Structure

Generated assets are organized into `out/<platform>/<title>/<id>/`:

```text
out/<platform>/<title>/<id>/
├── course.md          # Illustrated Markdown document (default)
├── course.html        # Self-contained styled HTML document (default)
├── structured.json    # Full structured data (when formats includes json)
├── frames/            # Extracted slide keyframe images
│   ├── slide_0001.jpg
│   └── ...
├── audio.wav          # Extracted audio (16kHz mono WAV)
├── timeline.jsonl     # Timestamp-aligned event stream
├── meta.json          # Video title, author, duration metadata
└── media.mp4          # Downloaded video (local input is read in-place; cleaned up by default)
```

### Completion Summary Example

Upon completion, `course2md` outputs a comprehensive summary detailing paths, metrics, elapsed time, and resident memory usage (RSS):

```text
──────── course2md Complete ────────
Title: Introduction to Computer Science - Lecture 01
Output Directory: out/bilibili/Introduction to Computer Science - Lecture 01/BV1pb8o6yE8f

Documents:
  out/bilibili/Introduction to Computer Science - Lecture 01/BV1pb8o6yE8f/course.md
  out/bilibili/Introduction to Computer Science - Lecture 01/BV1pb8o6yE8f/course.html
Frames: out/bilibili/Introduction to Computer Science - Lecture 01/BV1pb8o6yE8f/frames/ (24 images)
Audio: out/bilibili/Introduction to Computer Science - Lecture 01/BV1pb8o6yE8f/audio.wav
Video: Cleaned up (pass --keep-video to preserve)
Timeline: out/bilibili/Introduction to Computer Science - Lecture 01/BV1pb8o6yE8f/timeline.jsonl

Statistics: 24 slides / 142 speech segments / 8930 characters
Elapsed: 47s
Peak Memory: 1406 MB (course2md) + max child process 59 MB (llama-server/ffmpeg)
Model Directory: /Users/username/.cache/course2md/models
───────────────────────────────────
```

---

## CLI Options

| Option | Description | Default |
| :--- | :--- | :--- |
| `-o, --out <DIR>` | Output root directory | `out` |
| `--provider <coreml/gpu/cpu>` | ASR backend: `coreml` (macOS arm64), `gpu` (non-Mac), `cpu` | Platform default |
| `--similarity <0~1>` | SSIM similarity threshold; **lower = more slides captured** | `0.85` |
| `--sample-interval <SEC>` | Frame sampling check interval in seconds | `1.0` |
| `--cooldown <SEC>` | Minimum seconds between two consecutive slide captures | `10.0` |
| `--roi <x1,y1-x2,y2>` | Region of interest for slide comparison (e.g. `40%,0%-100%,100%`) | Full frame |
| `--formats <FORMATS>` | Comma-separated output formats: `md,html,json` | `md,html` |
| `--threads <N>` | Number of ASR worker threads | `4` |
| `--max-speech <SEC>` | Maximum speech segment duration in seconds | `20.0` |
| `--keep-video` | Preserve downloaded/extracted `media.mp4` | Disabled |
| `--no-download` | Skip downloading (when `media.mp4` exists in directory) | Disabled |
| `--llm` | Force enable LLM subtitle polishing for this run | Disabled |
| `--no-llm` | Force disable LLM subtitle polishing for this run | Disabled |
| `--no-llm-hint` | Suppress post-run LLM suggestion hint | Disabled |
| `-v, --verbose` | Increase logging verbosity (use `-vv` for debug) | `info` |
| `-q, --quiet` | Quiet mode (errors only) | Disabled |

Display full help:

```bash
course2md --help
```

---

## Benchmarks

Measured on a **3-minute** 1080p recorded lecture video:

| Platform & Hardware | ASR Backend (`--provider`) | End-to-End Elapsed | Peak Memory (RSS) | Characteristics |
| :--- | :--- | :--- | :--- | :--- |
| **macOS arm64**<br>(Apple Silicon M-series) | `coreml`<br>*(Default)* | **47s** | Course2md ~1.4 GB | **Zero external dependencies**; utilizes Neural Engine + Metal; warm model load takes only ~5s |
| **macOS arm64**<br>(Apple Silicon M-series) | `gpu`<br>*(llama.cpp Metal)* | **15s** | Course2md ~20 MB<br>+ llama-server ~3.3 GB | Highest throughput; requires `llama.cpp` and 1.7B Q8 GGUF model |
| **Arch Linux**<br>(x86_64, 16-core) | `cpu` | **1m45s** | Course2md ~13 MB<br>+ llama-server ~3.5 GB | Pure CPU computation; runs on any standard x86_64 Linux environment |

---

## License

This project is licensed under the [MIT License](LICENSE).
