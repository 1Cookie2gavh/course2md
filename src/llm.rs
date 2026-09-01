//! LLM 字幕润色（可选，默认关闭）。
//!
//! 配置文件：`~/.config/course2md/config.toml`（XDG；Windows 为 `%APPDATA%\course2md\config.toml`）。
//! 支持 OpenAI 兼容 /chat/completions 端点；所有配置项均可被命令行覆盖。
//! 关闭时每次任务结束打印开启提示（可用配置项或 `--no-llm-hint` 关闭）。

use crate::timeline::TranscriptEvent;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
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
    /// 转换完成后自动生成视频总结并写入 md/html（需 enabled）
    pub summarize: bool,
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

/// LLM 润色并发数（批次相互独立；过高易触发上游限流）。
const CONCURRENCY: usize = 4;

/// 用 LLM 批量润色字幕；失败批次保留原文（润色失败不阻断转换）。
/// 批次间并发执行（波次式，每波 CONCURRENCY 路），显著缩短长视频润色耗时。
pub fn polish(mut events: Vec<TranscriptEvent>, s: &LlmSettings) -> Vec<TranscriptEvent> {
    let mut chunks: Vec<Vec<TranscriptEvent>> = events.chunks(BATCH).map(|c| c.to_vec()).collect();
    let batches = chunks.len();
    let pb = indicatif::ProgressBar::new(batches as u64);
    pb.set_style(
        indicatif::ProgressStyle::with_template(
            "{spinner:.green} llm {pos}/{len} [{bar:32.cyan/blue}] {msg}",
        )
        .unwrap()
        .progress_chars("##-"),
    );
    let warned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    for wave in chunks.chunks_mut(CONCURRENCY) {
        std::thread::scope(|scope| {
            for chunk in wave {
                let s = s.clone();
                let pb = pb.clone();
                let warned = std::sync::Arc::clone(&warned);
                scope.spawn(move || {
                    polish_chunk(&s, chunk, &warned);
                    pb.inc(1);
                });
            }
        });
    }
    pb.finish_and_clear();
    // 按原顺序拼回（并发结果已写回各自 chunk）
    events = chunks.into_iter().flatten().collect();
    events
}

/// 递归润色一个分块；整块失败（如推理模型 token 耗尽返回空）时拆半重试，保证尽力而为。
fn polish_chunk(
    s: &LlmSettings,
    chunk: &mut [TranscriptEvent],
    warned: &std::sync::atomic::AtomicBool,
) {
    if chunk.is_empty() {
        return;
    }
    // 带分段下标请求/校验：防止模型重排或漏项导致的静默错位
    let items: Vec<(usize, &str)> = chunk
        .iter()
        .enumerate()
        .map(|(i, e)| (i, e.text.as_str()))
        .collect();
    match chat(s, &items) {
        Ok(polished) => {
            let mut by_id: Vec<Option<String>> = vec![None; chunk.len()];
            let mut bad = false;
            for (id, text) in polished {
                if id >= chunk.len() || by_id[id].is_some() {
                    bad = true;
                    continue;
                }
                by_id[id] = Some(text);
            }
            let applied = by_id.iter().filter(|v| v.is_some()).count();
            if applied == 0 {
                warn_once(warned, "LLM 未返回任何有效条目，该批保留原文");
                return;
            }
            if bad {
                warn_once(warned, "LLM 返回部分条目异常，仅应用有效条目");
            }
            for (ev, new_text) in chunk.iter_mut().zip(by_id.into_iter()) {
                if let Some(new_text) = new_text {
                    if !new_text.is_empty() && new_text != ev.text {
                        ev.raw.get_or_insert_with(|| ev.text.clone());
                        ev.text = new_text;
                    }
                }
            }
        }
        Err(e) => {
            if chunk.len() <= 1 {
                warn_once(warned, &format!("LLM 润色失败（{e:#}），保留原文"));
                return;
            }
            tracing::debug!("LLM 批次失败，拆半重试（{} 段）", chunk.len());
            let mid = chunk.len() / 2;
            polish_chunk(s, &mut chunk[..mid], warned);
            polish_chunk(s, &mut chunk[mid..], warned);
        }
    }
}

fn warn_once(warned: &std::sync::atomic::AtomicBool, msg: &str) {
    use std::sync::atomic::Ordering;
    if !warned.swap(true, Ordering::Relaxed) {
        tracing::warn!("{msg}（后续同类问题不再重复提示）");
    } else {
        tracing::debug!("{msg}");
    }
}

/// 发一批（id, 文本）给 LLM，返回润色后的 (id, 文本) 列表。
fn chat(s: &LlmSettings, items: &[(usize, &str)]) -> Result<Vec<(usize, String)>> {
    validate(s)?;
    let payload: Vec<serde_json::Value> = items
        .iter()
        .map(|(i, t)| serde_json::json!({"id": i, "text": t}))
        .collect();
    let body = serde_json::json!({
        "model": s.model,
        "temperature": 0.0,
        "max_tokens": 16384,
        "response_format": {"type": "json_object"},
        "messages": [
            {"role": "system", "content": format!("{} 输出为 JSON 数组，每项形如 {{\"id\":序号,\"text\":润色后的文本}}，id 必须与输入一一对应。", effective_prompt(s))},
            {"role": "user", "content": serde_json::to_string(&payload)?},
        ],
    });
    let content = send_chat(&s, &body).context("LLM 请求失败")?;
    parse_id_text_pairs(&content).with_context(|| {
        let hint = if content.trim().is_empty() {
            "content 为空（推理模型可能耗尽 token 预算，将自动拆半重试）".to_string()
        } else {
            format!("{:.200}", content)
        };
        format!("LLM 响应不是 id/text JSON 数组: {hint}")
    })
}

pub(crate) fn send_chat(s: &LlmSettings, body: &serde_json::Value) -> Result<String> {
    let resp = ureq::post(&endpoint(&s.base_url))
        .timeout(Duration::from_secs(300))
        .set("Content-Type", "application/json")
        .set("Authorization", &format!("Bearer {}", s.api_key))
        .send_json(body.clone())
        .context("LLM 请求失败")?;
    let v: serde_json::Value = resp.into_json().context("LLM 响应解析失败")?;
    Ok(v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string())
}

/// 从模型输出提取 [{"id":n,"text":"..."}]（容忍代码围栏、前后杂文、尾逗号与个别坏项）。
pub fn parse_id_text_pairs(content: &str) -> Option<Vec<(usize, String)>> {
    let start = content.find('[')?;
    let end = content.rfind(']')?;
    if end <= start {
        return None;
    }
    let slice = &content[start..=end];
    // 1) 严格解析
    if let Ok(v) = serde_json::from_str::<Vec<serde_json::Value>>(slice) {
        return parse_items(&v);
    }
    // 2) 清除尾逗号后重试
    let cleaned = remove_trailing_commas(slice);
    if cleaned != slice {
        if let Ok(v) = serde_json::from_str::<Vec<serde_json::Value>>(&cleaned) {
            return parse_items(&v);
        }
    }
    // 3) 宽容扫描：跳过坏项，收集合法 {"id":..,"text":".."}
    lenient_scan(slice)
}

fn parse_items(v: &[serde_json::Value]) -> Option<Vec<(usize, String)>> {
    let mut out = vec![];
    for item in v {
        let id = item.get("id")?.as_u64()? as usize;
        let text = item.get("text")?.as_str()?.to_string();
        out.push((id, text));
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn remove_trailing_commas(s: &str) -> String {
    let mut out = s.to_string();
    loop {
        let prev = out.clone();
        out = out.replace(",}", "}").replace(",]", "]");
        if out == prev {
            break;
        }
    }
    out
}

/// 逐个扫描 {"id":N,"text":"..."}，坏项跳过；能取到至少一项即返回。
fn lenient_scan(s: &str) -> Option<Vec<(usize, String)>> {
    let mut out: Vec<(usize, String)> = vec![];
    let mut rest = s;
    let mut guard = 0;
    while let Some(rel) = rest.find("\"id\"") {
        guard += 1;
        if guard > 10_000 {
            break;
        }
        let tail = &rest[rel..];
        // 找该对象起点
        let obj_start = tail.find('{')?;
        let mut depth = 0usize;
        let mut in_str = false;
        let mut esc = false;
        let mut end = None;
        let bytes = tail[obj_start..].as_bytes();
        for (k, &b) in bytes.iter().enumerate() {
            if in_str {
                if esc {
                    esc = false;
                } else if b == b'\\' {
                    esc = true;
                } else if b == b'"' {
                    in_str = false;
                }
                continue;
            }
            match b {
                b'"' => in_str = true,
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(obj_start + k + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end?;
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&tail[obj_start..end]) {
            if let (Some(id), Some(text)) = (
                v.get("id").and_then(|x| x.as_u64()).map(|x| x as usize),
                v.get("text").and_then(|x| x.as_str()).map(|s| s.to_string()),
            ) {
                out.push((id, text));
            }
        }
        rest = &tail[end.min(tail.len())..];
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
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
    mut cfg: crate::settings::ConfigFile,
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    disable_hint: bool,
) -> Result<crate::settings::ConfigFile> {
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

pub fn print_status(cfg: &crate::settings::ConfigFile) {
    let s = &cfg.llm;
    println!("配置文件：{}", crate::settings::config_path().display());
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

pub fn write_hint_note(path: &std::path::Path) {
    let msg = if crate::i18n::is_zh() {
        format!(
            "\n提示：可用 LLM 自动润色字幕（修正语气词与明显识别错误），运行 `course2md llm setup` 一键开启。\n配置文件：{}（加 --no-llm-hint 或在配置中设 disable_hint 可关闭本提示）\n",
            path.display()
        )
    } else {
        format!(
            "\nTip: enable LLM transcript polishing to fix filler words and obvious ASR errors — run `course2md llm setup`.\nConfig: {} (suppress this tip with --no-llm-hint or disable_hint in the config)\n",
            path.display()
        )
    };
    let _ = std::io::stderr().write_all(msg.as_bytes());
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
    fn parse_id_pairs_tolerates_fences() {
        let got = parse_id_text_pairs(
            "```json\n[{\"id\":0,\"text\":\"a\"},{\"id\":1,\"text\":\"b\"}]\n```",
        )
        .unwrap();
        assert_eq!(got, vec![(0, "a".into()), (1, "b".into())]);
        assert!(parse_id_text_pairs("没有数组").is_none());
        assert!(parse_id_text_pairs("[]").is_none());
    }
}
