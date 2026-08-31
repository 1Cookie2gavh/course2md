# course2md

把 YouTube、Bilibili 或本地网课/录屏视频转换为带截图的 Markdown / HTML 笔记。

[![Rust](https://img.shields.io/badge/Language-Rust-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey.svg)](#安装指南)

---

## 快速上手

传入在线视频 URL 或本地视频文件路径即可开始转换：

```bash
# 解析 B 站视频
course2md https://www.bilibili.com/video/BV1pb8o6yE8f

# 解析 YouTube 视频
course2md https://youtu.be/dQw4w9WgXcQ

# 解析本地课件/会议录屏
course2md ./lecture.mp4
```

> **说明**：首次运行时会自动下载语音识别模型（约 2.4GB），请保持网络连接稳定。

---

## 安装指南

运行 `course2md` 需要系统环境中具备以下外部依赖：
- `ffmpeg` & `ffprobe`（音视频提取与处理）
- `yt-dlp`（在线视频解析与下载）
- `llama-server`（由 `llama.cpp` 提供，负责本地 ASR 推理）

---

### macOS

推荐使用 Homebrew 一键安装所有依赖及程序：

```bash
# 1. 安装依赖
brew install ffmpeg yt-dlp llama.cpp

# 2. 安装 course2md
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash
```

---

### Arch Linux / CachyOS

```bash
# 1. 安装依赖（官方源及 CachyOS 均已包含 llama-cpp 与 llama-server）
sudo pacman -S ffmpeg yt-dlp llama-cpp

# 2. 安装 course2md
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash
```

<details>
<summary>若没有 AUR 环境，可从源码编译 llama.cpp</summary>

```bash
sudo pacman -S --needed base-devel cmake git
git clone https://github.com/ggml-org/llama.cpp.git
cmake -S llama.cpp -B llama.cpp/build -DLLAMA_CURL=OFF
cmake --build llama.cpp/build --config Release -j
sudo install -m755 llama.cpp/build/bin/llama-server /usr/local/bin/llama-server
```
</details>

---

### Debian / Ubuntu

```bash
# 1. 安装基础依赖与编译工具
sudo apt update
sudo apt install -y ffmpeg yt-dlp git cmake build-essential

# 2. 编译并安装 llama-server
git clone https://github.com/ggml-org/llama.cpp.git
cmake -S llama.cpp -B llama.cpp/build -DLLAMA_CURL=OFF
cmake --build llama.cpp/build --config Release -j
sudo install -m755 llama.cpp/build/bin/llama-server /usr/local/bin/llama-server

# 3. 安装 course2md
curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash
```

---

### Windows

在 **PowerShell** 中推荐使用 `winget` 安装依赖：

```powershell
winget install --id Gyan.FFmpeg -e
winget install --id yt-dlp.yt-dlp -e
winget install --id ggml.llamacpp -e
```

也可以使用 Scoop 或 Chocolatey 安装依赖。无论采用何种方式，请确保 `ffmpeg`、`ffprobe`、`yt-dlp`、`llama-server.exe` 均已加入系统 `PATH` 环境变量。

```powershell
# Scoop 方式
scoop install ffmpeg yt-dlp

# Chocolatey 方式
choco install ffmpeg yt-dlp
```

**安装 course2md**：
1. 前往 [Releases](https://github.com/mizorewww/course2md/releases) 下载 `course2md-windows-x86_64.exe`。
2. 重命名为 `course2md.exe` 并将其所在目录加入系统 `PATH`。

---

### 从源码构建

需要安装 Rust 稳定版（Stable）工具链：

```bash
git clone https://github.com/mizorewww/course2md.git
cd course2md
cargo install --path .
# 或仅构建 Release 二进制文件
cargo build --release
```

`install.sh` 脚本提供了 `macos-arm64`、`macos-x86_64`、`linux-x86_64`、`linux-aarch64` 架构的预编译版本，默认安装至 `~/bin`。

---

## 模型管理

`course2md` 使用针对多语言优化的高性能 **Qwen3-ASR GGUF** 模型。

模型会在首次转换任务时自动拉取，也可以通过子命令手动管理：

```bash
# 提前下载模型
course2md models download

# 查看本地模型缓存状态
course2md models list
```

**模型默认保存路径**：
- **macOS / Linux**：`~/.cache/course2md/models/`
- **Windows**：`%LOCALAPPDATA%\course2md\models\`

---

## 输出目录结构

转换产物将按 `out/<平台>/<标题>/<编号>/` 格式自动归档：

```text
out/<平台>/<标题>/<编号>/
├── course.md          # 图文混排 Markdown 文档（默认生成）
├── course.html        # 独立排版 HTML 页面（默认生成）
├── structured.json    # 结构化数据（指定 --formats 包含 json 时生成）
├── frames/            # 文稿中引用的幻灯片/关键帧截图
│   ├── slide_0001.jpg
│   └── ...
├── audio.wav          # 提取的音频（16kHz 单声道 WAV）
├── timeline.jsonl     # 带时间戳对齐的原始识别序列
├── meta.json          # 视频标题、作者、时长等元数据
└── media.mp4          # 下载的视频（本地文件输入时不复制；默认转换完成后自动删除）
```

任务完成后，终端会详细打印生成文稿、截图、音频、视频及时间线路径，汇总统计（截图数/语音段数/字数）、总耗时，以及两项峰值内存占用（course2md 进程 RSS + 最大子进程如 llama-server/ffmpeg 等），让资源开销清晰透明。

---

## LLM 字幕润色（可选）

`course2md` 支持在 ASR 转写完成后，调用大语言模型（LLM）对字幕文本进行自动化校对与润色。

- **润色目标**：修正语气词/口头禅（如「呃」、「嗯」、「这个那个」等）、重复字词、明显的同音错别字与专有名词拼写；**不增删实质内容、不翻译、不改变原意**。
  - *示例*：`我我干了什么呢？我在，我这是我的Neo Vim` → `我干了什么呢？我在，这是我的Neo Vim`
- **兼容接口**：支持任意 OpenAI 兼容的 `/chat/completions` 端点（如 DeepSeek、GLM、OpenAI 或本地部署的 vLLM / Ollama 等）。
- **容错保证**：按 20 段语音合并批次并发起请求（`temperature=0`）。若某批次请求失败或响应解析异常，将自动回退保留 ASR 原始文本并给出警告，**绝不阻断整体转换流程**。

### 快捷配置与命令

通过 `llm` 子命令可以快速完成配置与管理（默认处于关闭状态）：

```bash
# 交互式配置并开启（缺省项提示输入，按回车可保留已配置项，保存后自动测试连通性）
course2md llm setup

# 也可以直接通过命令行参数非交互式配置
course2md llm setup --base-url https://api.deepseek.com/v1 --api-key sk-xxxx --model deepseek-chat

# 查看当前 LLM 配置状态（API Key 自动打码）
course2md llm status

# 暂时关闭 LLM 润色功能（保留已配置的凭据与端点）
course2md llm disable
```

### 配置文件

配置保存于本地 TOML 文件中：
- **macOS / Linux**：`~/.config/course2md/config.toml`（遵循 `$XDG_CONFIG_HOME`）
- **Windows**：`%APPDATA%\course2md\config.toml`

文件结构示例：

```toml
[llm]
enabled = true                               # 是否开启润色（默认 false）
base_url = "https://api.deepseek.com/v1"     # OpenAI 兼容 API 地址（缺省协议自动补充 https://）
api_key = "sk-xxxx"                          # API 密钥
model = "deepseek-chat"                      # 模型名称
prompt = ""                                  # 自定义校对提示词（可选，留空使用内置提示词）
disable_hint = false                         # 是否永久关闭任务结束时的 LLM 开启提示（默认 false）
```

### 命令行运行时覆盖

配置文件中的所有项均可在执行具体转换任务时通过命令行即时覆盖，无需修改配置文件：

```bash
# 本次运行强制启用 / 禁用 LLM
course2md https://... --llm
course2md https://... --no-llm

# 临时指定其他模型或端点
course2md https://... --llm --llm-base-url https://api.deepseek.com/v1 --llm-api-key sk-xxxx --llm-model deepseek-chat

# 自定义校对提示词
course2md https://... --llm --llm-prompt "请校对以下字幕..."
```

### 结束提示

当 LLM 功能处于关闭状态时，每次转换任务完成后终端会打印一行关于可开启 LLM 润色的提示。若不需要该提示，可通过以下任一方式关闭：
- 单次运行添加 `--no-llm-hint` 参数。
- 在配置文件中设置 `disable_hint = true`，或运行 `course2md llm setup --disable-hint`。

---

## 常用参数

| 参数 | 说明 | 默认值 |
| :--- | :--- | :--- |
| `-o <目录>` | 指定输出根目录 | `out` |
| `--similarity <0~1>` | SSIM 画面相似度阈值；**数值越低截图越多** | `0.85` |
| `--cooldown <秒>` | 连续两张截图之间的最短间隔时间（秒） | `10` |
| `--formats <格式>` | 输出格式，逗号分隔，可选 `md,html,json` | `md,html` |
| `--provider <cpu/gpu>` | 指定推理后端；默认由 llama.cpp 自动调用 GPU | `gpu` |
| `--keep-video` | 保留下载或提取的原始 `media.mp4` 文件 | 关闭 |
| `--llm` | 本次运行启用 LLM 字幕润色（覆盖配置文件） | 关闭 |
| `--no-llm` | 本次运行禁用 LLM 字幕润色（覆盖配置文件） | 关闭 |
| `--no-llm-hint` | 本次运行关闭任务结束时的 LLM 开启提示 | 关闭 |

查看完整参数与选项列表：

```bash
course2md --help
```

---

## 开源协议

本项目基于 [MIT License](LICENSE) 开源。详见仓库根目录下的 [LICENSE](LICENSE) 文件。
