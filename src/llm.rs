//! LLM 字幕润色（可选，默认关闭）。
//!
//! 配置文件：`~/.config/course2md/config.toml`（XDG；Windows 为 `%APPDATA%\course2md\config.toml`）。
//! 支持 OpenAI 兼容 /chat/completions 端点；所有配置项均可被命令行覆盖。
//! 关闭时每次任务结束打印开启提示（可用配置项或 `--no-llm-hint` 关闭）。

use crate::timeline::TranscriptEvent;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const DEFAULT_PROMPT: &str = "你是字幕校对器。修正语音识别文本中明显的错误\
（错别字、同音字、专有名词拼写）与不通顺的语气词（如\"呃\"\"嗯\"\"这个那个\"等口头禅），\
使文字自然、书面化。不增删实质内容、不翻译、不改原意、保持原语言。\
输出与输入逐条对应的 JSON 字符串数组，不要输出任何其他内容。";

/// 每次请求合并的语音段数。
const BATCH: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct LlmSettings {
    pub enabled: bool,
    /// OpenAI 兼容 base URL，如 https://api.deepseek.com/v1
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// 覆盖默认校对提示词
    pub prompt: Option<String>,
    /// 关闭「可开启 LLM」的结束提示
    pub disable_hint: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct ConfigFile {
    pub llm: LlmSettings,
}

pub fn config_path() -> PathBuf {
    crate::config::config_dir().join("config.toml")
}

pub fn load_config() -> ConfigFile {
    let p = config_path();
    if !p.is_file() {
        return ConfigFile::default();
    }
    match std::fs::read_to_string(&p) {
        Ok(s) => toml::from_str(&s).unwrap_or_default(),
        Err(_) => ConfigFile::default(),
    }
}

pub fn save_config(cfg: &ConfigFile) -> Result<PathBuf> {
    let p = config_path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&p, toml::to_string_pretty(cfg)?)?;
    // 配置含 API Key，收紧权限（Windows 依赖用户目录 ACL）
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(p)
}

/// base_url -> 完整 chat/completions URL。
pub fn endpoint(base_url: &str) -> String {
    let b = base_url.trim().trim_end_matches('/');
    if b.ends_with("/chat/completions") {
        b.to_string()
    } else {
        format!("{b}/chat/completions")
    }
}

/// 校验配置可直接使用。
pub fn validate(s: &LlmSettings) -> Result<()> {
    if s.base_url.trim().is_empty() {
        bail!("llm.base_url 未配置，请运行 course2md llm setup");
    }
    if s.model.trim().is_empty() {
        bail!("llm.model 未配置，请运行 course2md llm setup");
    }
    Ok(())
}

/// 用 LLM 批量润色字幕；失败批次保留原文（润色失败不阻断转换）。
pub fn polish(mut events: Vec<TranscriptEvent>, s: &LlmSettings) -> Vec<TranscriptEvent> {
    let batches = events.chunks_mut(BATCH).len();
    let pb = indicatif::ProgressBar::new(batches as u64);
    pb.set_style(
        indicatif::ProgressStyle::with_template(
            "{spinner:.green} llm {pos}/{len} [{bar:32.cyan/blue}] {msg}",
        )
        .unwrap()
        .progress_chars("##-"),
    );
    let mut warned = false;
    for chunk in events.chunks_mut(BATCH) {
        pb.inc(1);
        let texts: Vec<&str> = chunk.iter().map(|e| e.text.as_str()).collect();
        match chat(s, &texts) {
            Ok(polished) if polished.len() == chunk.len() => {
                for (ev, t) in chunk.iter_mut().zip(polished) {
                    if !t.is_empty() {
                        ev.text = t;
                    }
                }
            }
            Ok(_) => warn_once(&mut warned, "LLM 返回条数与输入不符，该批保留原文"),
            Err(e) => warn_once(&mut warned, &format!("LLM 润色失败（{e:#}），保留原文")),
        }
    }
    pb.finish_and_clear();
    events
}

fn warn_once(warned: &mut bool, msg: &str) {
    if !*warned {
        tracing::warn!("{msg}（后续同类问题不再重复提示）");
        *warned = true;
    } else {
        tracing::debug!("{msg}");
    }
}

/// 发一批文本给 LLM，返回润色后的文本数组。
fn chat(s: &LlmSettings, texts: &[&str]) -> Result<Vec<String>> {
    validate(s)?;
    let body = serde_json::json!({
        "model": s.model,
        "temperature": 0.0,
        "max_tokens": 4096,
        "messages": [
            {"role": "system", "content": effective_prompt(s)},
            {"role": "user", "content": serde_json::to_string(texts)?},
        ],
    });
    let resp = ureq::post(&endpoint(&s.base_url))
        .timeout(Duration::from_secs(180))
        .set("Content-Type", "application/json")
        .set("Authorization", &format!("Bearer {}", s.api_key))
        .send_json(body)
        .context("LLM 请求失败")?;
    let v: serde_json::Value = resp.into_json().context("LLM 响应解析失败")?;
    let content = v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();
    parse_text_array(&content).with_context(|| format!("LLM 响应不是 JSON 数组: {content:.200}"))
}

/// 从模型输出中提取 JSON 字符串数组（容忍 ```json 围栏与前后杂文）。
pub fn parse_text_array(content: &str) -> Option<Vec<String>> {
    let start = content.find('[')?;
    let end = content.rfind(']')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&content[start..=end]).ok()
}

/// 空白提示词视为未设置，回落到内置提示词。
fn effective_prompt(s: &LlmSettings) -> &str {
    s.prompt
        .as_deref()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or(DEFAULT_PROMPT)
}

/// 用最小请求验证端点与凭据可用。
pub fn test_connection(s: &LlmSettings) -> Result<()> {
    validate(s)?;
    let body = serde_json::json!({
        "model": s.model,
        "max_tokens": 8,
        "messages": [{"role": "user", "content": "只回复两个字符：ok"}],
    });
    let resp = ureq::post(&endpoint(&s.base_url))
        .timeout(Duration::from_secs(60))
        .set("Authorization", &format!("Bearer {}", s.api_key))
        .send_json(body)
        .context("连接失败")?;
    let v: serde_json::Value = resp.into_json().context("响应解析失败")?;
    let text = v["choices"][0]["message"]["content"].as_str().unwrap_or("");
    println!("端点返回：{}", text.trim());
    Ok(())
}

/// `llm setup`：交互式补齐缺失项并写盘。
pub fn setup_interactive(
    mut cfg: ConfigFile,
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    disable_hint: bool,
) -> Result<ConfigFile> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    let mut line = String::new();

    // 回车保留当前值；hint 为展示用的掩码（如 api_key 已配置时）。
    let mut ask = |prompt: &str, current: &str, hint: &str| -> String {
        line.clear();
        let _ = write!(out, "{prompt}");
        if !hint.is_empty() {
            let _ = write!(out, "[回车保留 {hint}] ");
        }
        let _ = write!(out, ": ");
        let _ = out.flush();
        if stdin.read_line(&mut line).unwrap_or(0) == 0 {
            return current.to_string();
        }
        let t = line.trim();
        if t.is_empty() {
            current.to_string()
        } else {
            t.to_string()
        }
    };

    cfg.llm.base_url = base_url.unwrap_or_else(|| {
        ask(
            "Base URL（OpenAI 兼容，如 https://api.deepseek.com/v1）",
            &cfg.llm.base_url,
            &cfg.llm.base_url,
        )
    });
    cfg.llm.api_key = api_key
        .unwrap_or_else(|| ask("API Key", &cfg.llm.api_key, if cfg.llm.api_key.is_empty() { "" } else { "已配置的 Key" }));
    cfg.llm.model = model.unwrap_or_else(|| ask("模型名（如 deepseek-chat）", &cfg.llm.model, &cfg.llm.model));
    // 容错：没写 scheme 时补 https://
    if !cfg.llm.base_url.is_empty() && !cfg.llm.base_url.contains("://") {
        cfg.llm.base_url = format!("https://{}", cfg.llm.base_url.trim());
    }
    cfg.llm.disable_hint = disable_hint;
    cfg.llm.enabled = true;
    Ok(cfg)
}

pub fn print_status(cfg: &ConfigFile) {
    let s = &cfg.llm;
    println!("配置文件：{}", config_path().display());
    println!("  LLM 润色：{}", if s.enabled { "已开启" } else { "已关闭" });
    println!("  base_url：{}", if s.base_url.is_empty() { "-" } else { &s.base_url });
    let key_disp = if s.api_key.is_empty() {
        "-".to_string()
    } else {
        format!("{}...（已隐藏）", &s.api_key[..s.api_key.len().min(6)])
    };
    println!("  api_key ：{key_disp}");
    println!("  model   ：{}", if s.model.is_empty() { "-" } else { &s.model });
    println!("  结束提示：{}", if s.disable_hint { "已关闭" } else { "开启" });
    if !s.enabled && !s.disable_hint {
        println!("（运行 course2md llm setup 可开启）");
    }
}

pub fn write_hint_note(path: &Path) {
    let _ = std::io::stderr().write_all(
        format!(
            "\n提示：可用 LLM 自动润色字幕（修正语气词与明显识别错误），运行 `course2md llm setup` 一键开启。\n配置文件：{}（加 --no-llm-hint 或在配置中设 disable_hint 可关闭本提示）\n",
            path.display()
        )
        .as_bytes(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_join() {
        assert_eq!(
            endpoint("https://api.deepseek.com/v1"),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint("https://api.x.com/v1/"),
            "https://api.x.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint("https://api.x.com/v1/chat/completions"),
            "https://api.x.com/v1/chat/completions"
        );
    }

    #[test]
    fn parse_array_tolerates_fences() {
        assert_eq!(
            parse_text_array("```json\n[\"a\",\"b\"]\n```").unwrap(),
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(parse_text_array("结果：[\"你奸\"]").unwrap(), vec!["你奸"]);
        assert!(parse_text_array("没有数组").is_none());
    }
}
