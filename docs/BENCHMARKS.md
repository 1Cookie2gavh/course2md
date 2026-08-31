# macOS Apple Silicon Benchmarks / macOS Apple Silicon 性能基准

Reproduce with `packaging/bench-mac.sh` (speed + power via `powermetrics`). 复现：`packaging/bench-mac.sh`。

## Methodology / 方法

- **Machine / 机型**: Apple Silicon (arm64), macOS, release build
- **Input / 输入**: 3-minute 1080p lecture clip (Chinese speech), local file / 3 分钟 1080p 中文课程片段（本地文件）
- **Power / 功率**: `powermetrics` @ 1 Hz (`cpu_power,gpu_power,ane_power`), averaged over the run / 全程平均
- **Idle baseline / 空闲基线**: CPU ≈ 1.7 W, GPU ≈ 0.26 W — subtract for workload-only figures / 减去即纯工作负载功率

## Results / 结果（3-min video）

| Backend / 后端 | Wall / 总耗时 | ASR only / 纯识别 | CPU | GPU | ANE | Peak memory / 峰值内存 |
|---|---:|---:|---:|---:|---:|---|
| `coreml` + qwen3 0.6B **(default / 默认)** | 48 s | 45.9 s | 4.0 W | 0.2 W | 3.6 W | 1.41 GB (in-process / 进程内) |
| `coreml` + whisper large-v3-turbo | 86 s | 85.2 s | 13.2 W | 0.2 W | 0.4 W | 1.51 GB (in-process / 进程内) |
| `gpu` (llama.cpp Metal, Qwen3-ASR 1.7B Q8) | **12 s** | 10.5 s | 4.2 W | **17.6 W** | — | 26 MB + 3.3 GB child / 子进程 |
| `cpu` (llama.cpp, same model) | 27 s | 25.6 s | **20.6 W** | 0.9 W | — | 26 MB + 4.8 GB child / 子进程 |
| `api` (cloud STT / 云端) | ~10 s† | — | < 1 W | — | — | negligible / 可忽略 |

† Network-bound, provider-dependent. Audio leaves the machine / 取决于网络与提供商；音频会上传。

Derived totals over the run (power × time, for reference only) / 全程能量参考值（功率×时间）：
coreml-qwen3 ≈ 375 J · gpu-llama ≈ 263 J · cpu-llama ≈ 581 J · coreml-whisper ≈ 1194 J（含 ~2 W 空闲基线）。

## Takeaways / 结论

- **Battery-friendliest / 最省电**: `coreml`+qwen3 — Neural Engine carries the load at ~3.6 W sustained; total SoC power stays under 8 W. 适合笔记本电池场景。
- **Fastest / 最快**: `gpu` (llama.cpp + Metal) — ~4× faster end-to-end, at the cost of GPU bursts (17.6 W) and an external `llama-server` (~3.3 GB model in a child process).
- **whisper-turbo on CoreML**: slower & hotter here (decoder largely on CPU for short VAD segments); best suited to long-form audio with its 30 s windowing. In our Chinese sample qwen3 transcribed noticeably better / 中文课程 qwen3 明显更好。
- **First-run downloads / 首次下载**: coreml qwen3 ≈ 1–2 GB, whisper-turbo ≈ 1.5 GB → `~/Library/Caches/qwen3-speech/`; llama.cpp GGUF 2.4 GB → `~/.cache/course2md/models/`.

## Re-run / 复现

```bash
cargo build --release
sudo -v   # powermetrics needs root
packaging/bench-mac.sh path/to/video.mp4 [out-dir]
```
