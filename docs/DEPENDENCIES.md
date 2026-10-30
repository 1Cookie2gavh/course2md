# 依赖与发布说明（本 fork / Web 版）

本文件说明运行/构建依赖，以及我们发布 GitHub Release 的流程。官方上游的 v2 桌面端
（Tauri → GPUI）是另一条产品线，本 fork **不合并桌面端**，只按需挑选核心修复。

## 1. 运行时依赖

| 依赖 | 说明 |
|---|---|
| `ffmpeg` / `ffprobe` | 必需：抽音频、切段、抽帧、合并（发布包已内置） |
| `yt-dlp` | 必需（在线视频源）：下载与字幕探测。YouTube 新版强烈建议安装 **deno**（yt-dlp 默认 JS 运行时），否则部分格式/签名不可用；B 站建议先 `course2md login-bilibili` |
| `llama-server` + Qwen3-ASR GGUF | 本地语音识别需要（GPU 需 NVIDIA 驱动；模型约 2.4GB，首次 `course2md models download`） |
| 平台字幕 | 有平台字幕时不需要本地 ASR（B 站登录后可走 AI 字幕快路径） |
| 可选凭据 | 云端 STT Key（`[asr_api]`）、LLM Key（润色/总结）、B 站登录态（`auth/bilibili.cookies.txt`） |

> 发布包（win64 zip）已内置 ffmpeg / yt-dlp / llama-server 及运行库；模型与登录态不随包分发。

## 2. 构建依赖

- **Rust stable**（edition 2024，建议 ≥1.85；当前使用 1.98）+ **MSVC 链接器**（Visual Studio Build Tools）。
- Cargo 依赖（见 `Cargo.toml`）：anyhow、tokio、clap、serde/serde_json、image、ureq、
  base64、toml、tracing/tracing-subscriber、indicatif、image-compare、libc、dialoguer、
  qrcode、tiny_http。
- 国内网络建议配置 crates 镜像（如 rsproxy sparse index）并保留本地 cargo 缓存以支持离线构建。

## 3. 发布流程（Web 版）

```bash
cargo clippy --offline --all-targets -- -D warnings
cargo test --offline --all-targets
cargo build --offline --release
```

1. 二进制部署到**工作区之外的独立目录**（如 `D:\course2md-app\bin`）。
   注意：某些受限/沙箱环境会按“exe 所在位置”限制写盘，程序放在工作区内会导致写输出目录被拒。
2. 重新打包 Release 资产：
   - `course2md-win64-*.zip`：完整版（含 llama/CUDA）与精简版（仅 Web+字幕路径）；
   - 源码包：`git archive` 生成；
   - 更新 `资产说明.md` 的 SHA-256 与 `RELEASE_NOTES.md`。
3. 打 tag（如 `v1.3.0-c2m.N`）并推送：`git push origin main --tags`。
4. GitHub Release：上传三个 zip + 粘贴 Release Notes 正文。

## 4. 与官方桌面版（上游 v2）的关系

- 上游 v2 的桌面端依赖（Tauri/GPUI、系统 webview/图形栈、各平台打包工具链）与我们的
  Web 版完全不同，二者互不依赖；
- 本 fork 保持 v1.3.0 核心 + Web 增强，**不整仓合并 v2**，避免引入桌面端依赖与冲突；
- 需要时只移植核心修复（例如 LLM 契约、GPU 卸载控制、`--json` 事件流等）。
