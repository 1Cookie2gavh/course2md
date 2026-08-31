# course2md 设计文档

> 把一门网课视频（YouTube / Bilibili）变成一份「截图 + 语音转写」按时间顺序排列的课程文字版本。

## 1. 目标与非目标

**目标**

1. 一条命令完成：下载 → 场景截图 → 语音识别 → 时间线合并 → 输出 `course.md` / `course.html` / `structured.json`。
2. 全本地推理（无云端 API）：ASR 用 Qwen3-ASR（sherpa-onnx ONNX），VAD 用 Silero VAD。
3. Rust 异步编排（tokio）；yt-dlp / ffmpeg 一律走 subprocess，不硬造 API。
4. 每个阶段产物落盘、可单独重跑（渐进调试，断点友好）。

**非目标（明确不做，避免过度设计）**

- 不做说话人分离（diarization）、字幕翻译、LLM 摘要。
- 不做 GPU/CUDA 配置（sherpa-onnx CPU 已够：官方 RTF ≈ 0.1–0.15，100 分钟课 ≈ 10–15 分钟识别）。
- 不做多视频批量/队列/守护进程；一次一个 URL。
- 不做 GUI。

## 2. 技术选型

| 环节 | 选择 | 理由 |
|---|---|---|
| 编排 | Rust + tokio | 异步 subprocess 编排，类型安全的数据契约 |
| 下载 | yt-dlp (subprocess) | YouTube/Bilibili 站点规则变化频繁，yt-dlp 社区维护，别自己写 |
| 视频分析 | ffmpeg (subprocess) | `select=gt(scene,X)` 场景检测 + 精确 `-ss` 抽帧，稳定且快 |
| 截图去重 | dHash（自实现，~40 行）| 参考项目用 OpenCV+SSIM；我们只需「和上一张保留帧像不像」，64-bit dHash + 汉明距离足够，零重依赖 |
| VAD | Silero VAD（sherpa-onnx 内置） | 用户指定；模型仅 2.3MB |
| ASR | Qwen3-ASR（sherpa-onnx Rust crate） | 用户指定 Qwen3-ASR；sherpa-onnx crate ≥1.13.5 原生支持 `OfflineQwen3ASRModelConfig`（PR #3399，已测 0.6B/1.7B），静态链接开箱即用。whisper.cpp 被否决：一个 FFI 里拿不到「VAD + Qwen3」组合 |
| 渲染 | 手写模板 | md/html/json 都是简单线性结构，不引模板引擎 |

## 3. 管线与数据流

```
                        ┌────────────────┐
URL ──► yt-dlp ───────►│ media.mp4      │ + meta.json (title/uploader/duration/webpage_url)
                        └───┬────────┬───┘
                            │        │
              ┌─────────────┘        └──────────────┐
              ▼ (tokio::join! 并行)                 ▼
   ffmpeg scene-detect pass                ffmpeg -ac 1 -ar 16000
   (select=gt(scene,τ) + metadata=print)   → audio.wav (PCM s16le)
              │                                 │
   候选时间点 + 冷却去抖                        Silero VAD
              │                                 │
   逐点 -ss 精确抽帧 → frames/slide_NNNN.jpg     语音段 [start,end]
              │                                 │
   ROI(可选) + dHash 汉明距离去重                Qwen3-ASR 逐段离线解码
              │                                 │
        FrameEvent[]                     TranscriptEvent[]
              └─────────────┬───────────────────┘
                            ▼
                      Timeline merger（按时间合并成 Section[]）
                            │
                      timeline.jsonl（中间产物，落盘）
                            │
              ┌─────────────┼──────────────┐
              ▼             ▼              ▼
          course.md     course.html    structured.json
```

### 3.1 数据契约（serde 类型，跨阶段序列化）

```rust
struct FrameEvent     { t: f64, image: PathBuf }          // image 相对 out 目录
struct TranscriptEvent{ start: f64, end: f64, text: String }
struct Section        { t: f64, image: PathBuf,
                        speech: Vec<TranscriptEvent> }     // 一张截图 + 该截图期间的语音
```

`timeline.jsonl`：每行一个 JSON 事件（`frame` / `speech`），按 `t` 升序；渲染器只消费它。

### 3.2 合并算法（简单可解释）

1. 截图按 `t` 排序（天然有序）。
2. 每条语音按**中点** `(start+end)/2` 归属到「时间 ≤ 中点的最后一张截图」；首张截图之前的语音归首张。
   - 用中点而非 start：翻页常发生在连续讲话中间，中点归属让跨页句子落在讲该页 majority 的时间上。
3. 依次输出 Section。

## 4. 模块划分（单 crate，无 workspace）

```
src/
  main.rs      // clap 入口，子命令 run / models
  cli.rs       // 参数定义
  config.rs    // PipelineConfig（阈值、路径、线程数）
  error.rs     // thiserror/anywhy 统一错误
  fetch.rs     // yt-dlp：--dump-json 元数据 + 下载（720p 上限）
  media.rs     // ffmpeg：音频抽取(16k mono wav)、时长探测
  scene.rs     // 场景检测 pass → 冷却去抖 → 抽帧 → ROI+dHash 去重
  imgHash.rs   // dHash + 汉明距离（纯 Rust, image crate 解码）
  asr.rs       // sherpa-onnx：Silero VAD → Qwen3 逐段解码（专用线程）
  timeline.rs  // 合并器 + timeline.jsonl 读写
  render.rs    // md / html / json 渲染
  models.rs    // 模型下载与缓存管理（models 子命令）
  pipeline.rs  // run 的编排：join!(scene, audio) → asr → merge → render
```

## 5. 关键实现决策

### 5.1 yt-dlp
- 元数据：`yt-dlp -J <url>` 一次（title / uploader / duration / webpage_url / extractor）。
- 下载：`-f "bv*[height<=720]+ba/b[height<=720]/b" -S ext:mp4:m4a --merge-output-format mp4`。
- 输出进 `<out>/media.mp4`（`-o` 固定名，`--no-playlist`）。
- 已存在 `media.mp4` 时跳过下载（断点重跑）。

### 5.2 场景检测（两遍法）
- **Pass 1（检测）**：`ffmpeg -i media.mp4 -an -vf "select='gt(scene,τ)',metadata=print:file=-" -f null -`
  解析 stdout 的 `pts_time`。τ 默认 0.35（可调 `--scene-threshold`）。检测时强制缩小解码（`scale=w=640`）提速。
- **去抖**：冷却期（默认 10s，`--cooldown`）内只保留分数更高的候选；首帧 t=0 必保留。
- **Pass 2（抽帧）**：对每个保留时间点 `ffmpeg -ss T -i media.mp4 -frames:v 1 -q:v 2 frames/slide_NNNN.jpg`（精确 seek）。
- **去重**：可选 `--roi x1,y1-x2,y2`（支持百分比）先裁剪，再 dHash 与上一保留帧比较，汉明距离 ≤ 6 视为重复丢弃。

### 5.3 ASR（sherpa-onnx，专用线程）
- sherpa C 指针包装不保证 `Send`：整个 ASR 阶段在 `std::thread` 内完成（构造 recognizer → 循环），进度经 mpsc channel 汇报，主线程只是 `await` 结束信号 → 本质上仍是异步友好的阻塞任务。
- VAD 参数默认：threshold 0.5、min_silence 0.5s、min_speech 0.25s、max_speech 20s（超长自动切分）。
- 每个语音段独立送 Qwen3 解码，得到 `(start, end, text)`；空文本段丢弃。
- `num_threads` 默认 4。

### 5.4 模型管理
- 缓存目录：`$XDG_CACHE_HOME/course2md` 或 `~/.cache/course2md/models/`。
- `course2md models download --size 1.7b|0.6b`：
  - `silero_vad.onnx` ← GitHub k2-fsa/sherpa-onnx asr-models release（2.3MB）
  - 1.7B int8 ← ModelScope `zengshuishui/Qwen3-ASR-onnx`（conv_frontend 48M + encoder.int8 314M + decoder.int8 2.0G + tokenizer）
  - 0.6B int8 ← GitHub release tar.bz2（~950MB，备用/快速路径）
- `run` 时若模型缺失给出明确指引（先跑 `models download`）。

### 5.5 输出
- `course.md`：标题 + 元信息 + 来源链接；每节 `## [mm:ss](源?t=sec)` + `![](frames/….jpg)` + 该节语音段落。
- `course.html`：单文件、内联 CSS、图片 `loading=lazy`、时间戳可点击跳转。
- `structured.json`：`{meta, sections[]}`，程序可消费。
- 时间戳跳转：bilibili `?t=SEC`，youtube `&t=SEC`，其余统一 `?t=SEC`。

## 6. 错误处理与进度
- 任何子进程失败：带 stderr 摘要报错退出（不重试—— yt-dlp 下载除外：对网络错误重试 2 次）。
- 每阶段打印耗时与产物统计（候选数→保留数；语音段数→字符数）。
- `RUST_LOG=debug` 打印子进程完整输出。

## 7. 测试
- 单元：dHash/汉明距离、ROI 解析、时间格式化、合并器归属（含空帧/空语音边界）。
- 集成验收：`https://www.bilibili.com/video/BV1pb8o6yE8f`（NJU 生成式软件工程 01，100 分钟）全流程跑通并人工抽查输出。

## 8. 提交策略（原子化）
1. `docs: 设计文档与 README`
2. `chore: cargo 骨架与依赖`
3. `feat(cli): 子命令与配置`
4. `feat(models): 模型下载与缓存`
5. `feat(fetch): yt-dlp 元数据与下载`
6. `feat(media): 音频抽取与探测`
7. `feat(scene): 场景检测与抽帧去重`
8. `feat(asr): Silero VAD + Qwen3 识别`
9. `feat(timeline): 时间线合并`
10. `feat(render): md/html/json 渲染`
11. `feat(pipeline): run 编排串联`
12. `test: 单元测试与验收记录`
