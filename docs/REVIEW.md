# course2md 代码审查报告

> 2026-09 全面审查。范围：`src/` 全部 21 个模块、`build.rs`、`native/apple-asr/`、`tests/`。
> 总体结论：代码质量在个人项目中属中上——子进程全部走参数数组（无 shell 注入面）、HTML 输出全部转义、checkpoint 语义设计扎实、配置全量预检。问题集中在 8 个真实 bug、若干补丁式 hack、几处该收口而未收口的重复抽象、一处名不副实的 i18n。

## 一、真实 Bug（正确性）

| # | 位置 | 问题 |
|---|------|------|
| B1 | `build.rs:30-41` | Swift 增量逻辑失效：stamp 文件存在即跳过 `swift build`，改 Swift 源码后链接的仍是旧 `libCAppleASR.a`，无任何警告 |
| B2 | `checkpoint.rs:100-116` + `218-224` | 崩溃恢复自我污染：`load_events` 容忍末行半截 JSON，但 append 打开后下一次 `record()` 把新行拼在残行后，形成中间损坏行 → 下次 resume 硬错误 bail，恢复机制失效 |
| B3 | `llm.rs:590,648` | api_key 按字节切片 `[..len.min(6)]`，非 ASCII key 在字符边界处 panic |
| B4 | `subtitle.rs:87-95` | HTML 实体反转义顺序错：`&amp;` 在 `&lt;` 前替换，`&amp;lt;` 被双重反转义成 `<` |
| B5 | `timeline.rs:81,247`、`asr.rs:264,407,610` | `partial_cmp().unwrap()` 对外部时间戳数据，NaN 直接 panic；`asr.rs:613` 的 NaN 防御写在 `unwrap()` 之后，永不可达 |
| B6 | `llm.rs:614` | `unsafe { libc::isatty(2) }`：检查的是 stderr 而 dialoguer 读 stdin；Windows 上 libc 无此函数（readme 声称支持 Windows）；标准库 `IsTerminal` 即可 |
| B7 | `summarize.rs:195-209` | 静音课件（合法的空转写）会把空 transcript 发给 LLM「总结」，纯幻觉输出；`split_chunks` 还会 push 空块 |
| B8 | `npu.rs:202-215` | worker 关闭三层叠加（POST shutdown → SIGTERM 进程组 → SIGKILL）且层间零等待，「优雅关闭」永远走不到；`kill(-pid)` 在 pid 复用时存在误杀无辜进程组窗口 |

## 二、架构问题

1. **ASR 四个 provider 分支是同一骨架的四份拷贝**（`asr.rs:53-141`）：open checkpoint → clone → spawn_blocking → finish → join。coreml 分支的 finish 写法已与其他三处 drift，是复制粘贴腐化的实证。应抽 `run_with_cp` helper。
2. **`PipelineConfig` 同时携带 `out_dir` 与 `out_root`**，构造时同值（`main.rs:63-74` 同段表达式复制两遍），真正的 `out_dir` 在 pipeline 内部才覆盖。读 config 的人会误以为它已是最终目录。
3. **默认值三处硬编码**：`main.rs:75-109`、`settings.rs:162-258`（`print_effective`，已漏 5 个字段）、TEMPLATE 注释。改默认值要记三处。
4. **i18n 名不副实**：`tr()` 全项目只被调一次；错误信息、日志、`main.rs` 的 println 全是硬编码中文；`i18n.rs` 的中文 help 平行表已漏 5 个参数和 2 个子命令，且查不到时静默跳过。属于半途而废的机制，二选一：补齐或砍掉。
5. **`print_summary` 同一函数内两种风格**：满屏 `tr(...)` 而 `"音频：{}"` 一行又是裸中文。
6. **`cfg.validate()` 被调两次**（`main.rs:291` 与 `pipeline.rs:16`），入口职责不清。

## 三、Hack / 补丁式代码

| 位置 | 问题 |
|------|------|
| `llm.rs:424-444` | response_format 降级重试不分错误类型：401 双发请求、429 放大限流。应只在 400 时降级 |
| `summarize.rs:350,417` | 总结的插入/幂等/删除全靠扫 `"\n## ["`、`</header>` 字面量；`strip_md_summary` 找不到下一个标题就删到 EOF，用户手工追加的内容会被 `--force` 一并删除 |
| `fetch.rs:99,129` | `.mp4.part` 双扩展名 + `format!("{}.mp4", tmp.display())`，非 UTF-8 路径下 `display()` 产生替换符导致路径损坏 |
| `media.rs:42-82` | ffprobe 输出按逗号+空白切的脆性文本解析，应改 `-of json` |
| `llm.rs:461-469` | 用 `SystemTime` 纳秒当退避抖动随机源，并发 worker 同时失败时抖动退化为同步 |
| `apple.rs:123-130` | 未知模型名静默归为 qwen3，与 `npu.rs`「不静默更换模型」的设计原则正相反 |
| `runtime.rs:154` | `free_port` TOCTOU（bind 后释放再让 server 抢）；`/health` 探测可能撞上无关服务被误判就绪 |
| `llm.rs:313-418` | 三级手写 JSON 修复解析器。有测试与 id 校验兜底，可接受；`clean_trailing_commas` 会误伤字符串字面量内的 `,}`，留 TODO |

## 四、该抽象而没抽象

- **tmp 目录模式四份重复**（`asr.rs` ×3、`npu.rs` ×1）：`temp_dir()+pid` + 吞错的 `create_dir_all` + 手动收尾。应写 RAII guard。
- **`run_api` 手写线程池**（`asr.rs:312-374`）：70 行、11 个 Arc clone 解构、`Result<_, String>` 拍平错误链。`std::thread::scope` + 借用可砍掉全部 Arc。
- **ASR 的 HTTP 路径没有重试**（`asr.rs:436,507`）：LLM 已有指数退避，同构问题同等待遇。
- **`llm.rs` 与 `summarize.rs` 各写一份请求体构造**：`max_tokens`/`temperature` 因此散在两处，应抽 `chat_body()` 收拢。
- **进程调用样板**（Command + status 检查 + cmd_error）在 scene/media/fetch 三处重复。
- **`probe_duration` 同步/异步逐字两份**（`media.rs:85-129`）。
- `summarize.rs` map-reduce 分段完全串行，超长视频最慢，应 2-4 路并发。

## 五、过度设计 / 超前预留

- `llm.rs:517` `parse_text_array` 无调用方死代码。
- `asr.rs:14` `AsrInput` 只消费一次的包装结构。
- `npu.rs:104-109` base64 分支从未被使用（删掉还缩小攻击面）；`npu.rs:34` Python 默认端口死代码。
- `summarize.rs` 的 `_meta` 参数传进来完全没用（应用起来或删除）。
- checkpoint 身份包含 `course2md_version`：任何 patch 升级都作废全部 ASR 进度，应换成独立 schema 版本。
- 3.8MB `native/apple-asr/mlx.metallib` 二进制提交在 git 仓库（build.rs 已有从 SPM 产物回退的逻辑）。

## 六、硬编码

| 位置 | 问题 |
|------|------|
| `asr.rs:126` | checkpoint identity 写死 `"qwen3-1.7b-gguf"`，真实模型文件名在 `models.rs`——两处真相源，换模型时 identity 不同步会导致旧 checkpoint 被错误复用 |
| `config.rs:309` | 环境变量写死 `OPENROUTER_API_KEY`，与可配置 `asr_api.base_url` 矛盾 |
| `config.rs:408-419` | `default_provider_hint` 语义怪异：有 NPU 且装了 llama-server 反而默认 Gpu；没装任何后端的 Linux 默认一个必然失败的 Gpu |
| `fetch.rs:64` / `subtitle.rs:114-129` | 语言偏好 `zh.*,en.*` 两处硬编码且互相隐式依赖 |
| 多处 | 超参数分散：`-28dB` VAD 阈值、`-q:v 2`、640 采样宽度、`max_tokens` 16384/448/256 三处不一致、四个不同超时常量 |

## 七、安全 / 健壮性杂项

- `settings.rs:82-94`：先按 umask 0644 写文件再 chmod 600，中间存在 key 明文可读窗口，且 chmod 失败被吞；`save` 全量重写 toml 抹掉用户手写注释。
- `--llm-api-key` / `--asr-api-key` 走命令行会进 shell history 与 ps，help 应提示。
- markdown 渲染对 LLM 输出不转义（`summarize.rs:292-314`）——良性注入，个人工具风险低，但应有注释说明是有意信任。
- `models.rs:140` 2.4GB 下载无超时、无重试。
- `scene.rs:222-233` 逐帧串行起 ffmpeg 抽帧，N 张幻灯片 N 次进程 spawn。

## 八、做得对、不要去动的部分

- 子进程调用全部参数数组，无命令注入面；HTML 输出全部过 `esc()`。
- checkpoint 的身份作废、空 chunk 记完成、中间损坏硬错误等语义设计恰当，测试覆盖好（唯一漏洞是 B2）。
- `ManagedChild` / `wait_ready` / `drain_stderr` 是抽对了的抽象。
- `validate()` 全量预检、`deny_unknown_fields`、配置解析失败硬错误不回落默认，均为正确取舍。

## 九、修复记录

以下修复已全部执行并验证（`cargo build` / `cargo test` 58+2 全绿 / `cargo clippy --all-targets` 零警告）。

### 真实 Bug（全部修复）

- **B1** build.rs stamp 判断已删，apple_native 下无条件 `swift build`（SPM 自增量）。
- **B2** checkpoint partial resume 打开 append 前先 `set_len` 截到最后完整行（缺尾换行时补 `\n`）；新增 2 个测试覆盖「恢复后继续写」路径。
- **B3** api_key 掩码改 `chars().take(6)`。
- **B4** `&amp;` 移到实体替换序列最后。
- **B5** 全部改 `f64::total_cmp`；字幕解析出口过滤非有限时间戳；asr.rs 不可达的 NaN 检查已删。
- **B6** 改 `std::io::stdin().is_terminal()`，删除 unsafe/libc。
- **B7** `summarize()` 入口判空 bail；`split_chunks` 空块兜底已删；新增测试。
- **B8** NPU worker 关闭改为 shutdown → 轮询 2s → 进程组 SIGTERM → ManagedChild Drop 兜底，删除立即 SIGKILL。

### 架构 / 抽象

- ASR 四分支抽 `run_with_cp`（asr.rs），finish 语义统一为「成功才 finish」。
- 新增 `runtime::TempWorkDir` RAII guard，替换 asr.rs ×3 + npu.rs ×1 手写临时目录。
- `run_api` 改 `std::thread::scope` + 借用，Arc 全删，错误换回 anyhow。
- ASR HTTP 路径加指数退避重试（4xx 不重试）；llama 路径复用共享 ureq Agent。
- llm.rs/summarize.rs 请求体构造统一到 `chat_body()`，`temperature`/`max_tokens` 常量收拢。
- 新增 `media::run_cmd` 进程调用 helper，scene/media/fetch 三处复用；`probe_duration` 同步/异步共享参数与解析函数。
- summarize map-reduce 改 4 路并发（`SUMMARIZE_CONCURRENCY`）。
- 默认值收敛为 `config.rs` 顶部 `pub const`，main/settings/TEMPLATE 共用；`config show` 补齐 5 个漏展示字段。
- llama checkpoint identity 改由 `models::llama_gguf_identity()` 单一真相源（含一致性测试）。
- `main.rs` 重复的 `cfg.validate()` 已删（pipeline 开头统一预检）；`out_root/out_dir` 重复表达式收敛。

### Hack 清除

- response_format 降级重试仅在 400（或非 401/429 的 4xx）时触发。
- 总结块改用 `<!-- course2md:summary -->` 哨兵注释：插入/幂等/删除统一走哨兵，缺闭合哨兵保守不删；新增 `contains_html_summary`；旧版无哨兵块不识别（已知取舍）。
- fetch.rs 路径拼接改 `OsString`；`fetch_subtitle` 命令失败 warn 带 stderr。
- `probe_video` 改 `-of json` + serde_json。
- 重试抖动改进程级 `AtomicU64` 打散；apple.rs 未知模型名报错不再静默归类。
- `wait_ready` 校验 `/health` 响应体（`"status":"ok"`），防止端口抢占误判。

### 过度设计清理

- 删除：`parse_text_array` 死代码、`AsrInput` 包装结构、NPU base64 分支与 Python 默认端口、vendored `mlx.metallib`（3.8MB，SPM 产物回退已验证可重建）、`let api = &api;` 等。
- checkpoint 身份改用 `CHECKPOINT_SCHEMA_VERSION = 2`（不再随 patch 版本作废；本次升级旧 checkpoint 失效一次，设计内行为）。
- **i18n 模块整体移除**（`tr()` 仅一处真实使用、help 平行表已漂移且静默失败）：输出统一中文，CLI help 保持英文 derive，`src/i18n.rs` 删除。
- `_meta` 参数改为实际使用（标题/UP主注入总结 prompt）。

### 硬编码治理

- `COURSE2MD_ASR_API_KEY` 取代 `OPENROUTER_API_KEY`（旧名兼容回落），readme 双语文档同步。
- 语言偏好收拢为 `subtitle::SUB_LANGS` 单常量，fetch.rs 引用。
- asr/npu/scene/llm 的超时、VAD 阈值、并发数、token 上限等全部提为文件顶部带注释 const。
- `default_provider_hint` 补意图注释，doctor 措辞修正；NPU 别名表提为 const。
- `settings::save`：Unix 下 `OpenOptions::mode(0o600)` 一步建成 + 覆盖前 `.bak` 备份。
- cli help 补充：API key 命令行参数会进 shell history 的提示；`--asr-model` 后端约束说明。

### 遗留 / 已知取舍

- `config show` 的 f64 显示 `1` 而非 `1.0`（纯展示差异）。
- 总结块首次插入的定位锚仍是 `"\n## ["`/`</header>`（正文结构锚点，非总结探测）；旧版无哨兵的总结块不会被识别，重复跑会产生双块。
- `llm.rs` 的 `clean_trailing_commas` 理论上会误伤字符串字面量内的 `,}`（仅在严格解析失败后启用，有 id 校验兜底，留 TODO）。
- `scene.rs` 采样时间戳为理想值 `t = i * interval`（注释已说明漂移原因与可接受性）。
- `slide_{:04}.jpg` 超过 9999 张后字典序乱（极端长课，暂不处理）。
