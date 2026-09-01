# Changelog

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 与
[语义化版本](https://semver.org/lang/zh-CN/)。

## [1.1.0] — 2026-09-01

### 新增

- **LLM 视觉润色** `--llm-vision` / 配置 `vision = true`：按节附幻灯片截图，
  模型参照画面纠正技术词汇拼写；端点不支持图片时该批自动降级纯文本。
  `llm setup` 交互式询问模型视觉能力（脚本化调用不阻塞）。
  （Implements #5，感谢 @mizorewww）
- **纯语气词条目删除**：默认提示词允许对纯语气词/口头禅条目返回空文本，
  该条将被删除且原文保留在 `raw` 字段溯源。（Implements #5）
- `run.json` 记录 `llm_vision`。

### 修复

- **ASR 进度条不再被 llama-server 日志打穿**：llama-server stderr 此前直接
  继承终端，其每 chunk 的 slot timing 日志插在进度条重绘之间，导致进度条
  每次更新都新起一行。改为 piped + 后台 drain（尾部缓存进错误信息，debug
  可转发）；顺带修复 scene 采样 ffmpeg stderr 未 drain 的死锁隐患。
  （Fixes #4，感谢 @mizorewww）
- **`llm setup` / CoreML 模型选择支持方向键编辑**：裸 `read_line` 不处理
  方向键转义序列（←/→/Home/End 变字面字符）。改用 dialoguer。
  （Fixes #3，感谢 @mizorewww）

## [1.0.0] — 2026-09-01

首个正式版。本轮以「正确性审计 + 架构收敛」为主题：修复全部已知的
数据丢失 / 静默错误结果类缺陷，将字符串分发改为类型系统，把配置错误
提前到毫秒级暴露，并新增运行溯源与环境体检。

### 新增

- **平台字幕优先的转写来源** `--transcript-source auto|subtitle|asr`
  （默认 `auto`）：平台人工字幕 > 平台自动字幕 > 本地 ASR；命中字幕时
  完全不抽音频、不加载模型。本地视频支持同名 `.srt`/`.vtt` sidecar。
  解析器支持 SRT/VTT、多行 cue、行内标签清理、滚动字幕去重。
  （Implements #1，感谢 @kernerydel）
- **`course2md doctor`**：环境体检——ffmpeg/ffprobe/yt-dlp/llama-server/uv
  可用性、平台后端（CoreML/NPU）、配置文件（含权限告警）、模型缓存状态。
- **`run.json` 运行溯源**：每次运行在输出目录记录版本、转写来源、
  provider/模型、统计与耗时（原子写，不含凭据）。
- `--no-resume` / `--resume` 互斥参数与三态解析。
- `structured.json` 增加 `schema_version` 与 `generator{name,version}`；
  `Section` 增加 `end`（下一段起点 / 媒体时长）。

### 修复

- **`--no-resume` 此前完全无效**（声明了但从未读取），用户以为重跑全部，
  实际可能复用旧进度。
- **`--no-download` 不再删除用户已有视频**：只有「本次运行真正下载的」
  媒体文件才会在结束时清理（此前只要未开 `--keep-video` 就会误删）。
- **checkpoint 运行身份**：新增 `.asr_identity`（版本/provider/模型/
  max_speech）。此前换模型后断点续跑会静默混用两个模型的转写。
  1.0 前的无身份旧进度自动作废重算。
- **空语音 chunk 也计入 checkpoint**：静音段此前每次续跑都重复识别。
- **checkpoint 写盘失败不再标记完成**；`.asr_done` 原子写且错误不再吞掉；
  中间行损坏从静默跳过改为硬错误（末行半截仍按崩溃残留容忍）；
  `resume=false` 时清档，杜绝重复运行导致的 asr.jsonl 叠加双份文本。
- **NPU 后端三连修**：
  1. 【致命】内嵌 Python worker 存在语法错误（f-string 字面换行），
     `--provider npu` 自合入起无法启动；
  2. Whisper 管线硬编码 `<|zh|>`，英文课被强制按中文解码产生幻觉转写，
     改为语言自动检测；
  3. Qwen 下载/加载失败时静默回退 Whisper（模型族都变了），改为硬错误。
  （感谢 @little-q-exist 的报告催生了对 NPU 路径的完整审查）
- **GGUF 下载尊重 `HF_ENDPOINT` 镜像**（此前硬编码 huggingface.co，
  README 宣称的镜像方案仅 CoreML 路径生效）。（Fixes #2，感谢 @little-q-exist）
- **相似度阈值文案方向修正**：实际语义为「阈值越高越敏感、截图越多」，
  CLI 帮助 / 配置模板 / 双语 README 全部改正。
- **预检校验**：`max_speech=0` 会导致切分算法 `clamp(min>max)` panic、
  `--formats` 拼错要到渲染阶段才报错、provider×模型不兼容（如 gpu 上
  指定 whisper）被静默忽略、provider=api 缺 key 要切完音频才发现——
  全部提前到任何昂贵操作之前毫秒级失败。
- 配置文件路径展开 `~`（此前会真的创建 `./~/` 目录）；
  配置未知字段（如 `similairty`）直接报错而非静默忽略；
  TOML `provider="npu"` 此前被手写校验拒绝，现随类型系统天然支持。

### 变更 / 重构

- `provider` / `slide_mode` / `formats` 由裸字符串改为 typed enum
  （`AsrProvider` / `SlideMode` / `OutputFormat`），CLI、TOML、运行时
  共用同一套类型；非法值在解析期即失败。
- checkpoint 协议 v2：运行身份 + 空 chunk 记录 + 写失败不标记完成 +
  损坏策略 + 重复行去重。
- 新增 `runtime` 模块：`ManagedChild`（kill-on-drop，任何 `?` 早退不
  泄漏子进程——修复 NPU worker 泄漏）+ 健康轮询同时监视子进程秒退 +
  统一 `which`/`free_port`。
- 四个 ASR 后端的重复 chunk 循环收敛为 `asr::run_chunks`；云端 API
  并发路径删除 `Vec<Arc<AtomicBool>>` 与未使用的双层 Option 结果表。
- 时间线：跨截图边界的文本切点吸附到最近句读/空格（±6 字符窗口），
  不再词中切断；字符守恒保持不变。
- 首次运行模型选择器与 README 去掉「绝无漏字/识别极准/零漏句」等
  不可复现的营销话术，改为客观差异描述；readme.md 移除 3 份重复章节。
- CoreML 后端移除无依据的 `unsafe impl Send`（句柄从不跨线程移动）。

### 迁移提示

- 1.0 前生成的 ASR checkpoint（无身份标记）会在下次运行时自动作废重算
  一次，属预期行为（保证转写来源可追溯）。
- `--transcript-source auto` 成为默认：有平台字幕的课程将不再走本地
  ASR。如需旧行为请传 `--transcript-source asr`。

## [0.8.1]

- 全系统优先推荐 Qwen3-ASR 1.7B 并详细标注模型错漏分析。

## [0.8.0]

- Intel NPU 硬件加速识别（`--provider npu`）。

## [0.7.0]

- ASR checkpoint / 断点续跑。

## [0.6.0]

- 数据正确性大修（场景三状态机 / 能量感知切分 / 图文边界对齐）。

## [0.5.0]

- 云端 STT、Whisper CoreML、CLI 国际化、macOS 功耗基准。