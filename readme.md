# course2md

把一门网课视频变成「截图 + 语音转写」按时间排列的课程文字版。

贴链接或本地文件即可：

```bash
course2md https://www.bilibili.com/video/BV1pb8o6yE8f
course2md https://youtu.be/dQw4w9WgXcQ
course2md ./lecture.mp4
```

## 依赖

- Rust、`ffmpeg`、`yt-dlp`（macOS：`brew install ffmpeg yt-dlp`）
- Apple GPU 路径还需要项目里的 `.venv`：`uv venv && uv pip install qwen-asr torch`

```bash
cargo install --path .
```

首次：

```bash
course2md models download          # ONNX int8 + Silero（CPU 后备）
# Qwen3-ASR 1.7B 官方权重放到 ~/.cache/course2md/models/Qwen3-ASR-1.7B/
```

## CLI

```
course2md <url|文件> [选项]
course2md models download [--size 1.7b|0.6b]
course2md models list
```

| 选项 | 默认 | 说明 |
|---|---|---|
| `-o, --out` | `out/<视频id>/` | 输出目录 |
| `--provider` | `mps` | `mps`（Apple GPU）/ `cpu` / `coreml` |
| `--keep-video` | 关 | 保留 media.mp4 |
| `-v / -q` | info | 更详细 / 只报错 |

输出：

```
out/BV1pb8o6yE8f/
├── frames/slide_0001.jpg ...
├── timeline.jsonl
├── course.md
├── course.html
└── structured.json
```

## 设计

见 [docs/DESIGN.md](docs/DESIGN.md)。

## License

MIT
