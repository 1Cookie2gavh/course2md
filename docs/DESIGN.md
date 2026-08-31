# course2md 设计

course2md 是单一用途的命令行工具：把 YouTube、Bilibili 或本地网课视频转换为按时间组织的截图与文字稿。

## 管线

```text
URL ── yt-dlp ─┐
本地文件 ──────┴─> media.mp4 + meta.json
                         │
              ┌──────────┴──────────┐
              │                     │
              ▼                     ▼
  ffmpeg 定时灰度采样        ffmpeg 提取 16 kHz
  ROI + SSIM 比较            单声道 audio.wav
  cooldown 去抖              silencedetect 静音分段
  精确抽帧到 frames/         最长时长切分
              │                     │
              │                     ▼
              │          llama-server + Qwen3-ASR
              │          GGUF 本地语音识别
              └──────────┬──────────┘
                         ▼
              按时间合并截图与语音
                         │
              timeline.jsonl
                         │
          course.md / course.html / structured.json
```

截图与音频提取并行执行。SSIM 只比较当前采样画面和上一张保留画面；低于 `--similarity` 时视为新画面，`--cooldown` 控制最短截图间隔。语音由 ffmpeg `silencedetect` 划分，再逐段提交给本机 `llama-server`。ASR 模型是约 2.4GB 的 Qwen3-ASR GGUF，缺失时自动下载。

时间线合并时，每段语音按时间中点归入当时最近的一张截图。默认生成 Markdown 和 HTML；JSON 通过 `--formats` 启用。完成后默认删除 `media.mp4`，其他中间产物保留。

## 模块

```text
src/
  main.rs      CLI 入口与模型子命令
  cli.rs       参数定义
  config.rs    运行配置、缓存和输出路径
  fetch.rs     yt-dlp 元数据与视频下载
  media.rs     ffprobe 探测、ffmpeg 音频提取
  scene.rs     SSIM 采样、ROI、冷却与抽帧
  asr.rs       silencedetect 分段、llama-server 生命周期与识别
  models.rs    Qwen3-ASR GGUF 下载与状态
  timeline.rs  截图/语音合并及 JSONL
  render.rs    Markdown、HTML、JSON 输出
  pipeline.rs  全流程编排、清理与完成摘要
  error.rs     外部命令检查与错误包装
```

Rust 负责数据契约和异步编排；`yt-dlp`、`ffmpeg`、`ffprobe`、`llama-server` 均作为外部进程调用。模型和识别结果留在本地，不依赖云端 API。

## 非目标

- 不做说话人分离、翻译、摘要或内容改写。
- 不做 GUI、服务端、队列或批量任务系统。
- 不接管 CUDA、Metal 等运行时配置；GPU 能力由用户安装的 llama.cpp 决定，`--provider cpu` 可强制 CPU。
- 不内建视频网站适配、媒体编解码或模型推理实现；这些职责分别交给 yt-dlp、ffmpeg 和 llama.cpp。
