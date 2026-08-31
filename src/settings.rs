//! 配置文件（~/.config/course2md/config.toml）。
//! 优先级：命令行参数 > 配置文件 > 内置默认值。

use anyhow::Result;
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
    pub roi: Option<String>,
    pub threads: Option<i32>,
    pub provider: Option<String>,
    pub max_speech: Option<f32>,
    pub formats: Option<Vec<String>>,
    pub model_dir: Option<PathBuf>,
    pub keep_video: Option<bool>,
    pub no_download: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct ConfigFile {
    pub defaults: Defaults,
    pub llm: crate::llm::LlmSettings,
}

pub fn config_path() -> PathBuf {
    crate::config::config_dir().join("config.toml")
}

pub fn load() -> ConfigFile {
    let p = config_path();
    if !p.is_file() {
        return ConfigFile::default();
    }
    match std::fs::read_to_string(&p) {
        Ok(s) => toml::from_str(&s).unwrap_or_default(),
        Err(_) => ConfigFile::default(),
    }
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
# 识别后端：coreml（仅 macOS Apple Silicon，默认）| gpu（Metal/CUDA via llama.cpp）| cpu
#provider = "coreml"
# 单段语音最长秒数（过长会切分）
#max_speech = 20.0
# 输出格式：md / html / json
#formats = ["md", "html"]
# 模型目录（llama.cpp GGUF；CoreML 模型缓存在 ~/Library/Caches/qwen3-speech/）
#model_dir = "~/.cache/course2md/models"
# 保留下载的视频 media.mp4
#keep_video = false

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
    println!("配置文件：{}", config_path().display());
    println!("[defaults]");
    let s = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".into());
    println!("  out            : {}", d.out.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "out".into()));
    println!("  similarity     : {}", d.similarity.map(|v| v.to_string()).unwrap_or_else(|| "0.85".into()));
    println!("  sample_interval: {}", d.sample_interval.map(|v| v.to_string()).unwrap_or_else(|| "1.0".into()));
    println!("  cooldown       : {}", d.cooldown.map(|v| v.to_string()).unwrap_or_else(|| "10.0".into()));
    println!("  roi            : {}", s(&d.roi));
    println!("  threads        : {}", d.threads.map(|v| v.to_string()).unwrap_or_else(|| "4".into()));
    println!("  provider       : {}", s(&d.provider));
    println!("  max_speech     : {}", d.max_speech.map(|v| v.to_string()).unwrap_or_else(|| "20.0".into()));
    println!("  formats        : {}", d.formats.clone().map(|f| f.join(",")).unwrap_or_else(|| "md,html".into()));
    println!("  model_dir      : {}", d.model_dir.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "(内置缓存目录)".into()));
    println!("  keep_video     : {}", d.keep_video.unwrap_or(false));
    println!("[llm]");
    println!("  enabled        : {}", cfg.llm.enabled);
    println!("  model          : {}", if cfg.llm.model.is_empty() { "-" } else { &cfg.llm.model });
}
