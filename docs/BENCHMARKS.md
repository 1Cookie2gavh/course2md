# Benchmarks / 性能与功耗基准

Apple Silicon (macOS) 与 Intel Core Ultra NPU (Linux) 实测数据。
Measured on Apple Silicon (macOS) and Intel Core Ultra NPU (Linux).

---

## 1. macOS (Apple Silicon arm64)

Reproduce with `packaging/bench-mac.sh` (speed + power via `powermetrics`). 复现：`packaging/bench-mac.sh`。

### Methodology / 方法

- **Machine / 机型**: Apple Silicon (arm64), macOS, release build
- **Input / 输入**: 3-minute 1080p lecture clip (Chinese speech), local file / 3 分钟 1080p 中文课程片段（本地文件）
- **Power / 功率**: `powermetrics` @ 1 Hz (`cpu_power,gpu_power,ane_power`), averaged over the run / 全程平均
- **Idle baseline / 空闲基线**: CPU ≈ 1.7 W, GPU ≈ 0.26 W — subtract for workload-only figures / 减去即纯工作负载功率

### Results / 结果（3-min video）

| Backend / 后端 | Wall / 总耗时 | ASR only / 纯识别 | CPU | GPU | ANE | Peak memory / 峰值内存 |
|---|---:|---:|---:|---:|---:|---|
| `coreml` + qwen3 0.6B **(default / 默认)** | 47 s | 46.4 s | 6.7 W | 0.2 W | 3.5 W | 1.41 GB (in-process / 进程内) |
| `coreml` + whisper large-v3-turbo | 87 s | 85.3 s | 15.3 W | 0.3 W | 0.4 W | 1.51 GB (in-process / 进程内) |
| `gpu` (llama.cpp Metal, Qwen3-ASR 1.7B Q8) | **13 s** | 11.2 s | 4.7 W | **16.0 W** | — | 26 MB + 3.3 GB child / 子进程 |
| `cpu` (llama.cpp, same model) | 26 s | 25.6 s | **21.2 W** | 0.6 W | — | 26 MB + 4.8 GB child / 子进程 |
| `api` (cloud STT / 云端) | ~10 s† | — | < 1 W | — | — | negligible / 可忽略 |

† Network-bound, provider-dependent. Audio leaves the machine / 取决于网络与提供商；音频会上传。

Derived totals over the run (power × time, for reference only) / 全程能量参考值（功率×时间）：
coreml-qwen3 ≈ 493 J · gpu-llama ≈ 269 J · cpu-llama ≈ 567 J · coreml-whisper ≈ 1387 J（含 ~2 W 空闲基线）。

### Takeaways / 结论

- **Battery-friendliest / 最省电**: `coreml`+qwen3 — Neural Engine carries the load at ~3.5 W sustained; total SoC power stays low. 适合笔记本电池场景。
- **Fastest / 最快**: `gpu` (llama.cpp + Metal) — ~4× faster end-to-end, at the cost of GPU bursts (16.0 W) and an external `llama-server` (~3.3 GB model in a child process).
- **whisper-turbo on CoreML**: slower & hotter here (decoder largely on CPU for short VAD segments); best suited to long-form audio with its 30 s windowing. In our Chinese sample qwen3 transcribed noticeably better / 中文课程 qwen3 明显更好。
- **First-run downloads / 首次下载**: coreml qwen3 ≈ 1–2 GB, whisper-turbo ≈ 1.5 GB → `~/Library/Caches/qwen3-speech/`; llama.cpp GGUF 2.4 GB → `~/.cache/course2md/models/`.

---

## 2. Linux (Intel Core Ultra NPU)

### Methodology / 方法

- **Machine / 机型**: Intel Core Ultra X7 358H (16 cores, Intel AI Boost NPU 50xx), Arch Linux / CachyOS x86_64
- **Input / 输入**: 同上 3 分钟 1080p 教学课程视频（本地文件）
- **Runtime / 运行时**: OpenVINO GenAI NPU (`openvino-intel-npu-plugin`, `intel-npu-driver`)

### Results / 结果（3-min video）

| Backend / 后端 | Wall / 总耗时 | ASR only / 纯识别 | 实时倍率 (xRT) | Peak memory / 峰值内存 | 特点 |
|---|---:|---:|---:|---:|---|
| **`npu` (Whisper Large-v3 Turbo INT8)** | **16 s** | **15.1 s** | **~12x** | 18 MB + 557 MB child | **速度与精度平衡最佳**；比纯 CPU 快 6 倍，内存占用降低 84%，专有名词识别极准 |
| **`npu` (Whisper Tiny FP16)** | **6 s** | **4.6 s** | **~39x** | 18 MB + 180 MB child | **极速模式**；3 分钟音频 4.6 秒完成识别，极低内存与功耗 |
| **`cpu` (llama.cpp Qwen3-ASR 1.7B Q8)** | 100 s (1m40s) | 98.4 s | ~1.8x | 18 MB + 3548 MB child | 纯 CPU 计算；内存开销大（3.5GB），耗时明显较长 |

### Takeaways / 结论

- **Intel NPU 性能巨大提升**：搭载 Intel Core Ultra（Meteor Lake / Lunar Lake / Arrow Lake / Panther Lake）的笔记本开启 `--provider npu` 后，Whisper Large-v3-Turbo 达到 12 倍实时速度（15 秒识别 3 分钟音频），比 16 核 CPU 纯算快 **6 倍以上**。
- **功耗与内存友好**：NPU 计算功耗极低（避免 CPU 满载风扇狂转），且显存/内存占用仅 500MB 出头（纯 CPU llama.cpp 需要 3.5GB）。
- **模型切换自由**：支持 `--asr-model turbo` (默认)、`--asr-model base`、`--asr-model tiny` 以及任意 HuggingFace 上的 OpenVINO Whisper 导出模型。
