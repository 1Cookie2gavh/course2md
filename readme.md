# course2md

把网课视频转成带截图的文字稿。

```bash
course2md https://www.bilibili.com/video/BV1pb8o6yE8f
course2md https://youtu.be/dQw4w9WgXcQ
course2md ./lecture.mp4
```

需要本机已安装 `ffmpeg` 和 `yt-dlp`。首次使用先下载模型：

```bash
course2md models download
```

结果写在 `out/平台/标题/编号/`，例如：

```
out/bilibili/欢迎来到未来/BV1pb8o6yE8f/
├── frames/
├── course.md
├── course.html
└── structured.json
```

```
course2md --help
```
