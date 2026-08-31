# course2md

把 YouTube、Bilibili 或本地网课视频转成带截图的 Markdown / HTML 文字稿。

```bash
course2md <url或本地文件>
```

例如：

```bash
course2md https://www.bilibili.com/video/BV1pb8o6yE8f
course2md https://youtu.be/dQw4w9WgXcQ
course2md ./lecture.mp4
```

## 安装

运行需要 `ffmpeg`、`ffprobe`、`yt-dlp` 和 llama.cpp 提供的 `llama-server`。

### macOS

```bash
brew install ffmpeg yt-dlp llama.cpp
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash
```

也可以从源码安装 course2md：

```bash
git clone https://github.com/mizorewww/course2md.git
cd course2md
cargo install --path .
```

### Arch Linux 

```bash
sudo pacman -S ffmpeg yt-dlp
# CachyOS 等仓库可能已有：
sudo pacman -S llama-cpp
# 或 AUR：
# yay -S llama.cpp
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash
```

没有 AUR helper 时，可从源码安装 llama.cpp：

```bash
sudo pacman -S --needed base-devel cmake git
git clone https://github.com/ggml-org/llama.cpp.git
cmake -S llama.cpp -B llama.cpp/build -DLLAMA_CURL=OFF
cmake --build llama.cpp/build --config Release -j
sudo install -m755 llama.cpp/build/bin/llama-server /usr/local/bin/llama-server
```

### Debian / Ubuntu

```bash
sudo apt update
sudo apt install -y ffmpeg yt-dlp git cmake build-essential
git clone https://github.com/ggml-org/llama.cpp.git
cmake -S llama.cpp -B llama.cpp/build -DLLAMA_CURL=OFF
cmake --build llama.cpp/build --config Release -j
sudo install -m755 llama.cpp/build/bin/llama-server /usr/local/bin/llama-server
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash
```

### Windows

PowerShell 中推荐用 winget 安装依赖：

```powershell
winget install --id Gyan.FFmpeg -e
winget install --id yt-dlp.yt-dlp -e
winget install --id ggml.llamacpp -e
```

也可以用 Chocolatey 安装 `ffmpeg`、`yt-dlp`，或用 Scoop 安装这些依赖；无论采用哪种方式，请确认 `ffmpeg`、`ffprobe`、`yt-dlp`、`llama-server.exe` 都已加入 `PATH`。

```powershell
choco install ffmpeg yt-dlp
# 或
scoop install ffmpeg yt-dlp
```

然后从 [Releases](https://github.com/mizorewww/course2md/releases) 下载 `course2md-windows-x86_64.exe`，重命名为 `course2md.exe` 并加入 `PATH`；也可安装 Rust 后从源码编译：

```powershell
git clone https://github.com/mizorewww/course2md.git
cd course2md
cargo install --path .
```

### 从源码构建

需要稳定版 Rust 工具链：

```bash
git clone https://github.com/mizorewww/course2md.git
cd course2md
cargo install --path .
# 或只构建二进制
cargo build --release
```

`install.sh` 提供 `macos-arm64`、`macos-x86_64`、`linux-x86_64`、`linux-aarch64` 预编译版本，默认安装到 `~/bin`。

## 首次运行与模型

首次转换时会自动下载 llama.cpp 使用的 Qwen3-ASR GGUF 模型（约 2.4GB），请保持网络连接并不要中途退出。也可以提前下载或查看状态：

```bash
course2md models download
course2md models list
```

模型默认保存在：

- macOS / Linux：`~/.cache/course2md/models/`
- Windows：`%LOCALAPPDATA%/course2md/models/`

## 输出在哪里

结果按 `out/<平台>/<标题>/<编号>/` 归档：

```text
out/<平台>/<标题>/<编号>/
├── course.md          # 默认生成
├── course.html        # 默认生成
├── structured.json    # 使用 --formats md,html,json 时生成
├── frames/            # 文稿中的截图
├── audio.wav          # 提取后的音频
├── timeline.jsonl     # 带时间戳的中间结果
├── meta.json          # 视频元信息
└── media.mp4          # 默认完成后删除；--keep-video 可保留
```

任务完成后会打印文稿、截图、音频和视频路径，以及总耗时和本进程峰值内存（RSS）。

## 常用参数

| 参数 | 作用 |
|---|---|
| `-o <目录>` | 指定输出根目录，默认 `out` |
| `--keep-video` | 保留下载或复制的 `media.mp4` |
| `--provider cpu` | 强制 CPU 识别；默认由 llama.cpp 使用 GPU |
| `--formats md,html,json` | 选择输出格式，默认 `md,html` |
| `--similarity <0~1>` | SSIM 相似度阈值；**越低截图越多**，默认 0.85 |
| `--cooldown <秒>` | 两张新截图之间的最短间隔，默认 10 秒 |

完整参数：

```bash
course2md --help
```
