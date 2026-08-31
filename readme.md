# course2md

把网课视频转成带截图的文字稿。

```bash
course2md https://www.bilibili.com/video/BV1pb8o6yE8f
course2md https://youtu.be/dQw4w9WgXcQ
course2md ./lecture.mp4
```

macOS Apple Silicon 一键安装：

```bash
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | zsh
```

需要：`ffmpeg`、`yt-dlp`、`llama-server`。macOS：`brew install ffmpeg yt-dlp llama.cpp`。

第一次运行会自动下载识别模型（约 2.4GB），期间不要退出。也可手动：`course2md models download`。

结果在 `out/平台/标题/编号/`。

默认用 GPU（Apple Metal / NVIDIA CUDA，由本机 llama.cpp 决定）。`--provider cpu` 可强制 CPU。
