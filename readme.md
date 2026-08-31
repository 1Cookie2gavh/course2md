# course2md

把网课视频转成带截图的文字稿。

```bash
course2md https://www.bilibili.com/video/BV1pb8o6yE8f
course2md https://youtu.be/dQw4w9WgXcQ
course2md ./lecture.mp4
```

需要：`ffmpeg`、`yt-dlp`、`llama-server`（llama.cpp）。macOS：`brew install ffmpeg yt-dlp llama.cpp`。

```bash
course2md models download
```

结果在 `out/平台/标题/编号/`。

默认用 GPU（Apple Metal / NVIDIA CUDA，由本机 llama.cpp 决定）。`--provider cpu` 可强制 CPU。
