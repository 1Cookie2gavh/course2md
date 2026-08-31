# course2md

把一门网课视频（YouTube / Bilibili）变成一份「截图 + 语音转写」按时间顺序排列的课程文字版本。

```
URL ──► yt-dlp ──► ffmpeg(场景检测+抽帧) ─┐
                └─► ffmpeg(PCM 16k) ─► Silero VAD ─► Qwen3-ASR ─┐
                                                                ▼
                       ┌──────────── timeline merger ◄──────────┘
                       ▼
              timeline.jsonl ──► course.md / course.html / structured.json
```

- 全本地推理，无云端 API；Rust 异步编排，yt-dlp / ffmpeg 走 subprocess。
- ASR：Qwen3-ASR（sherpa-onnx，ONNX int8）+ Silero VAD。

## 安装

依赖：Rust ≥1.85、`ffmpeg`、`yt-dlp`（macOS：`brew install ffmpeg yt-dlp`）。

```bash
cargo install --path .
```

## 模型准备（首次必做）

```bash
course2md models download --size 1.7b   # Qwen3-ASR 1.7B int8 (~2.4GB) + Silero VAD
# 或 --size 0.6b  (~950MB，更快，精度略低)
```

模型缓存于 `~/.cache/course2md/models/`。

## 使用

```bash
course2md run "https://www.bilibili.com/video/BV1pb8o6yE8f" -o out/nju-01
```

常用参数：

| 参数 | 默认 | 说明 |
|---|---|---|
| `--scene-threshold` | 0.35 | ffmpeg scene 分数阈值，越小越敏感 |
| `--cooldown` | 10 | 两次截图最小间隔秒数 |
| `--roi x1,y1-x2,y2` | 无 | 去重时只比较该区域（支持 `25%,0%-100%,100%` 百分比）|
| `--hamming` | 6 | dHash 汉明距离 ≤ 此值视为重复帧 |
| `--threads` | 4 | ASR 推理线程数 |
| `--keep-video` | 关 | 保留下载的 media.mp4 |
| `--formats md,html,json` | 全部 | 输出格式 |

输出目录：

```
out/nju-01/
├── media.mp4          # (--keep-video 时保留)
├── audio.wav          # 16k mono PCM
├── meta.json          # 视频元数据
├── frames/slide_0001.jpg ...
├── timeline.jsonl     # 中间产物：frame/speech 事件流
├── course.md / course.html / structured.json
```

## 设计

见 [docs/DESIGN.md](docs/DESIGN.md)。

## License

MIT
