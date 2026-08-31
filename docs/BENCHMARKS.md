# macOS Apple Silicon Benchmarks

基准脚本：`packaging/bench-mac.sh`（速度 + 功耗，`powermetrics` 采样）。
Benchmarks for Apple Silicon: speed and power, measured by `packaging/bench-mac.sh`.

## Methodology

- **Machine**: Apple Silicon (arm64), macOS, release build (`cargo build --release`)
- **Input**: 3-minute 1080p lecture clip (Chinese speech), local file
- **Timing**: end-to-end wall clock; ASR-only time from logs
- **Power**: `powermetrics` @1 Hz (`cpu_power,gpu_power,ane_power`), averaged over the run
- **Baseline (idle)**: CPU ≈ 1.7 W, GPU ≈ 0.26 W, ANE = 0 W — subtract mentally for workload-attributable power
- Peak memory: `getrusage` of the tool process + largest child (`llama-server`)

## Results (3-min video)

| Backend | Wall | ASR only | Avg CPU | Avg GPU | Avg ANE | Energy* | Peak memory |
|---|---:|---:|---:|---:|---:|---:|---|
| `coreml` + qwen3 0.6B (default) | 48 s | 45.9 s | 4.0 W | 0.2 W | 3.6 W | **≈ 375 J** | 1.41 GB (in-process) |
| `coreml` + whisper large-v3-turbo | 86 s | 85.2 s | 13.2 W | 0.2 W | 0.4 W | ≈ 1194 J | 1.51 GB (in-process) |
| `gpu` (llama.cpp Metal, Qwen3-ASR 1.7B Q8) | **12 s** | 10.5 s | 4.2 W | 17.6 W | — | ≈ 263 J | 26 MB + 3.3 GB child |
| `cpu` (llama.cpp, same model) | 27 s | 25.6 s | 20.6 W | 0.9 W | — | ≈ 581 J | 26 MB + 4.8 GB child |
| `api` (cloud STT) | ~10 s† | — | <1 W | — | — | — | negligible |

\* Energy = Σ(avg power × wall time), includes the idle baseline (≈ 2 W × t); treat as an upper bound.
† Network-bound; depends on provider latency. Audio leaves the machine — not comparable on privacy.

## Takeaways

- **Efficiency**: `coreml`+qwen3 is the most frugal path — the Neural Engine does the heavy lifting at ~3.6 W sustained, total SoC power stays under 8 W. Best for laptops on battery.
- **Throughput**: `gpu` (llama.cpp + Metal) is ~4× faster end-to-end, at the cost of GPU bursts (17.6 W) and an external `llama-server` process holding a 3.3 GB model.
- **whisper-turbo on CoreML**: slower and hotter here (decoder largely on CPU for short VAD segments); it shines for long-form English audio with its 30 s windowing. For Chinese lectures qwen3 transcribed noticeably better in our sample.
- **First-run downloads**: coreml models (qwen3 ≈ 1–2 GB, whisper-turbo ≈ 1.5 GB) cache under `~/Library/Caches/qwen3-speech/`; llama.cpp GGUF (2.4 GB) under `~/.cache/course2md/models/`.

## Re-running

```bash
cargo build --release
sudo -v   # powermetrics needs root; the script re-prompts otherwise
packaging/bench-mac.sh path/to/video.mp4 [out-dir]
```

The script runs all four local backends back-to-back with isolated power sampling per run.
