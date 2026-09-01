//! 配置文件（~/.config/course2md/config.toml）。
//! 优先级：命令行参数 > 配置文件 > 内置默认值。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// `[defaults]`：命令行参数的默认值。全部可选，未设置的项回落内置默认。
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Defaults {
    pub out: Option<PathBuf>,
    pub similarity: Option<f64>,
    pub sample_interval: Option<f64>,
    pub cooldown: Option<f64>,
    pub slide_mode: Option<String>,
    pub stable_secs: Option<f64>,
    pub max_height: Option<u32>,
    pub roi: Option<String>,
    pub threads: Option<i32>,
    pub provider: Option<String>,
    /// coreml 后端的模型：qwen3 | whisper（首次使用可交互选择）
    pub asr_model: Option<String>,
    pub max_speech: Option<f32>,
    pub formats: Option<Vec<String>>,
    pub model_dir: Option<PathBuf>,
    pub keep_video: Option<bool>,
    pub no_download: Option<bool>,
    pub resume: Option<bool>,
}

/// 云端 STT（provider = "api"，OpenAI 兼容 /audio/transcriptions，如 OpenRouter）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct AsrApi {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

impl Default for AsrApi {
    fn default() -> Self {
        Self {
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key: String::new(),
            model: "qwen/qwen3-asr-flash-2026-02-10".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct ConfigFile {
    pub defaults: Defaults,
    pub llm: crate::llm::LlmSettings,
    pub asr_api: AsrApi,
}

pub fn config_path() -> PathBuf {
    crate::config::config_dir().join("config.toml")
}

/// 读取配置文件。文件不存在 → 默认值；存在但无法解析 → 硬错误（带位置信息），
/// 避免用户写错一个引号后静默回落默认、甚至开始下载 2.4GB 模型。
pub fn load() -> anyhow::Result<ConfigFile> {
    let p = config_path();
    if !p.is_file() {
        return Ok(ConfigFile::default());
    }
    let s = std::fs::read_to_string(&p)
        .with_context(|| format!("无法读取配置文件 {}", p.display()))?;
    toml::from_str(&s).with_context(|| {
        format!(
            "配置文件解析失败（修正后重试；本次不回退默认值）：{}",
            p.display()
        )
    })
}

pub fn save(cfg: &ConfigFile) -> Result<PathBuf> {
    let p = config_path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&p, toml::to_string_pretty(cfg)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(p)
}

/// `config init` 写入的带注释模板。
pub const TEMPLATE: &str = r#"# course2md 配置文件
# 优先级：命令行参数 > 本文件 > 内置默认值。
# 任何命令行参数（如 --similarity）都可以在这里设置默认值；
# 保持注释状态即使用内置默认。

[defaults]
# 输出根目录（其下按 平台/标题/编号 归类）
#out = "out"
# SSIM 画面相似度阈值，越低截图越多
#similarity = 0.85
# 每隔几秒检查一次画面
#sample_interval = 1.0
# 新截图之后至少间隔多少秒
#cooldown = 10.0
# 只比较画面中的区域，如 "40%,0%-100%,100%"
#roi = "40%,0%-100%,100%"
# 识别线程数
#threads = 4
# 识别后端推荐：
# - gpu: 强烈推荐！Metal (macOS) / CUDA (Linux) / Vulkan，加载 Qwen3-ASR 1.7B Q8，3分钟音频仅需13秒，专有名词与标点极准
# - npu: Intel Core Ultra NPU 硬件加速（Linux/Windows），高能效比，比纯 CPU 快 6.5 倍
# - coreml: macOS Apple Silicon 原生 CoreML / Neural Engine 模式，零外部依赖
# - cpu: 纯 CPU 运行 Qwen3-ASR 1.7B Q8，通用兜底
# - api: 云端 STT（OpenRouter），免本地模型下载
#provider = "gpu"

# 识别模型推荐 (各个后端通用)：
# - qwen3 (强烈推荐): Qwen3-ASR 1.7B，中文及技术课程首选，专有名词准，标点规范，绝无句尾吞字
# - whisper: Whisper Large-v3 Turbo，适合纯英文或多语种视频
#asr_model = "qwen3"

# 单段语音最长秒数（过长会切分，自动在静音低能量点切分并外补 0.25s 静音 padding）
#max_speech = 20.0
# 输出格式：md / html / json
#formats = ["md", "html"]
# 模型目录（llama.cpp GGUF；CoreML 模型缓存在 ~/Library/Caches/qwen3-speech/）
#model_dir = "~/.cache/course2md/models"
# 保留下载的视频 media.mp4
#keep_video = false

[asr_api]
# 云端 STT（--provider api，OpenAI 兼容 /audio/transcriptions；OpenRouter 聚合多模型）
#base_url = "https://openrouter.ai/api/v1"
#api_key = "sk-or-..."
#model = "qwen/qwen3-asr-flash-2026-02-10"
# 其他常用模型：openai/whisper-large-v3-turbo、qwen/qwen3-asr-1.7b

[llm]
# LLM 字幕润色（默认关闭）。运行 `course2md llm setup` 可交互式配置。
enabled = false
#base_url = "https://api.deepseek.com/v1"
#api_key = "sk-..."
#model = "deepseek-chat"
# 自定义校对提示词（留空用内置）
#prompt = ""
# 关闭任务结束时的 LLM 开启提示
#disable_hint = false
"#;

/// 打印生效配置（CLI 覆盖合并前，来自文件的值）。
pub fn print_effective(cfg: &ConfigFile) {
    let d = &cfg.defaults;
    println!("{}: {}", crate::i18n::tr("Config file", "配置文件"), config_path().display());
    println!("[defaults]");
    let s = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".into());
    println!("  out            : {}", d.out.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "out".into()));
    println!("  similarity     : {}", d.similarity.map(|v| v.to_string()).unwrap_or_else(|| "0.85".into()));
    println!("  sample_interval: {}", d.sample_interval.map(|v| v.to_string()).unwrap_or_else(|| "1.0".into()));
    println!("  cooldown       : {}", d.cooldown.map(|v| v.to_string()).unwrap_or_else(|| "10.0".into()));
    println!("  roi            : {}", s(&d.roi));
    println!("  threads        : {}", d.threads.map(|v| v.to_string()).unwrap_or_else(|| "4".into()));
    println!("  provider       : {}", s(&d.provider));
    println!("  asr_model      : {}", s(&d.asr_model));
    println!("  max_speech     : {}", d.max_speech.map(|v| v.to_string()).unwrap_or_else(|| "20.0".into()));
    println!("  formats        : {}", d.formats.clone().map(|f| f.join(",")).unwrap_or_else(|| "md,html".into()));
    println!("  model_dir      : {}", d.model_dir.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "(内置缓存目录)".into()));
    println!("  keep_video     : {}", d.keep_video.unwrap_or(false));
    println!("[asr_api]");
    println!("  base_url       : {}", cfg.asr_api.base_url);
    println!("  model          : {}", cfg.asr_api.model);
    println!("  api_key        : {}", if cfg.asr_api.api_key.is_empty() { "-" } else { "(已配置)" });
    println!("[llm]");
    println!("  enabled        : {}", cfg.llm.enabled);
    println!("  model          : {}", if cfg.llm.model.is_empty() { "-" } else { &cfg.llm.model });
}
