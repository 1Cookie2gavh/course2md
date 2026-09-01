//! LLM 字幕润色（可选，默认关闭）。
//!
//! 配置文件：`~/.config/course2md/config.toml`（XDG；Windows 为 `%APPDATA%\course2md\config.toml`）。
//! 支持 OpenAI 兼容 /chat/completions 端点；所有配置项均可被命令行覆盖。
//! 关闭时每次任务结束打印开启提示（可用配置项或 `--no-llm-hint` 关闭）。
//!
//! 视觉润色（`vision = true`）：按 Section 分批，每个请求附该节幻灯片截图，
//! 供模型校正技术词汇拼写（issue #5）；端点不支持图片时该批自动降级纯文本。

use crate::timeline::{Section, TranscriptEvent};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use std::time::Duration;

pub const DEFAULT_PROMPT: &str = "你是视频逐字稿校对器。输入的每一项是一段已按自然停顿组织的连续讲解。\
修正明显的语音识别错误（错别字、同音字、专有名词拼写），删除不影响原意的冗余口头填充，\
并修复不自然的断句和标点，使文字自然、书面化。不得概括、扩写、翻译、增删实质内容或改变原意；\
保持原语言。若某条内容仅由语气词、口头禅或无实义片段构成（如单独的\"啊\"、\"对吧\"），\
该条的 text 返回空字符串 \"\"（系统会删除该条）；有实质内容的条目不得删除。\
输出与输入逐条对应的 JSON 对象数组，每项形如 {\"id\":序号,\"text\":\"校对后的文本\"}，不要输出任何其他内容。";

/// 每次请求合并的语音段数。
const BATCH: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default, deny_unknown_fields)]
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
    /// 视觉润色：每个请求附对应幻灯片截图，辅助纠正技术词汇（模型须支持图片输入）
    pub vision: bool,
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

/// LLM 润色并发数（Section 间相互独立；过高易触发上游限流）。
const CONCURRENCY: usize = 4;

/// 对已合并的 Section 做润色（在 merge 之后调用）。
/// - 失败批次保留原文（润色失败不阻断转换）
/// - vision=true 且截图存在时，请求附该节幻灯片；带图失败自动降级纯文本重试一次
/// - 模型对纯语气词条目返回空 text → 该条被删除（issue #5）
/// - Section 间并发（波次式）；批次失败拆半递归重试，推理模型下更稳更快
pub fn polish_sections(sections: &mut [Section], frames_root: &Path, s: &LlmSettings) {
    let total: usize = sections
        .iter()
        .map(|sec| sec.speech.chunks(BATCH).len())
        .sum();
    let pb = indicatif::ProgressBar::new(total as u64);
    pb.set_style(
        indicatif::ProgressStyle::with_template(
            "{spinner:.green} llm {pos}/{len} [{bar:32.cyan/blue}] {msg}",
        )
        .unwrap()
        .progress_chars("##-"),
    );
    let warned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let vision_warned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    for wave in sections.chunks_mut(CONCURRENCY) {
        std::thread::scope(|scope| {
            for sec in wave {
                let s = s.clone();
                let pb = pb.clone();
                let frames_root = frames_root.to_path_buf();
                let warned = std::sync::Arc::clone(&warned);
                let vision_warned = std::sync::Arc::clone(&vision_warned);
                scope.spawn(move || {
                    polish_section(&s, &frames_root, sec, &pb, &warned, &vision_warned);
                });
            }
        });
    }
    pb.finish_and_clear();
}

/// 润色单个 Section（含视觉图片解析与纯语气词条目删除）。
fn polish_section(
    s: &LlmSettings,
    frames_root: &Path,
    sec: &mut Section,
    pb: &indicatif::ProgressBar,
    warned: &std::sync::atomic::AtomicBool,
    vision_warned: &std::sync::atomic::AtomicBool,
) {
    if sec.speech.is_empty() {
        return;
    }
    let image = if s.vision {
        let p = frames_root.join(&sec.image);
        p.is_file().then_some(p)
    } else {
        None
    };
    for chunk in sec.speech.chunks_mut(BATCH) {
        pb.inc(1);
        polish_chunk(s, chunk, image.as_deref(), warned, vision_warned);
    }
    // 删除「成功润色为空串」的纯语气词条目
    sec.speech.retain(|e| !e.text.trim().is_empty());
}

/// 递归润色一个分块；失败（含 id 集不匹配）时拆半重试，保证尽力而为。
fn polish_chunk(
    s: &LlmSettings,
    chunk: &mut [TranscriptEvent],
    image: Option<&Path>,
    warned: &std::sync::atomic::AtomicBool,
    vision_warned: &std::sync::atomic::AtomicBool,
) {
    if chunk.is_empty() {
        return;
    }
    let items: Vec<(usize, &str)> = chunk
        .iter()
        .enumerate()
        .map(|(i, e)| (i, e.text.as_str()))
        .collect();
    let r = match (chat(s, &items, image), image) {
        (Ok(v), _) => Ok(v),
        (Err(e), Some(_)) => {
            warn_once(
                vision_warned,
                &format!("带图润色失败（{e:#}），该批降级为纯文本重试"),
            );
            chat(s, &items, None)
        }
        (Err(e), None) => Err(e),
    };
    match r {
        Ok(polished) => {
            let mismatched = apply_polish(chunk, &polished);
            if mismatched {
                if chunk.len() > 1 {
                    split_and_retry(s, chunk, image, warned, vision_warned);
                } else {
                    warn_once(warned, "LLM 返回 id 集与输入不符，该批保留原文");
                }
            }
        }
        Err(e) => {
            if chunk.len() > 1 {
                split_and_retry(s, chunk, image, warned, vision_warned);
            } else {
                warn_once(warned, &format!("LLM 润色失败（{e:#}），保留原文"));
            }
        }
    }
}

/// 把批次一分为二递归重试（更小批次更易成功，如推理模型 token 耗尽）。
fn split_and_retry(
    s: &LlmSettings,
    chunk: &mut [TranscriptEvent],
    image: Option<&Path>,
    warned: &std::sync::atomic::AtomicBool,
    vision_warned: &std::sync::atomic::AtomicBool,
) {
    let mid = chunk.len() / 2;
    polish_chunk(s, &mut chunk[..mid], image, warned, vision_warned);
    polish_chunk(s, &mut chunk[mid..], image, warned, vision_warned);
}

/// 把 (id, 新文本) 应用到一批事件上；空字符串 = 删除该条（由调用方 retain）。
/// 返回 true = 返回集与输入不匹配（重排/缺项/重复），该批保留原文。
fn apply_polish(chunk: &mut [TranscriptEvent], polished: &[(usize, String)]) -> bool {
    let mut by_id: Vec<Option<&str>> = vec![None; chunk.len()];
    for (id, text) in polished {
        if *id >= chunk.len() || by_id[*id].is_some() {
            return true;
        }
        by_id[*id] = Some(text.as_str());
    }
    if by_id.iter().any(|v| v.is_none()) {
        return true;
    }
    for (ev, new) in chunk.iter_mut().zip(by_id) {
        let new = new.unwrap_or("");
        if new != ev.text {
            ev.raw.get_or_insert_with(|| ev.text.clone());
            ev.text = new.to_string();
        }
    }
    false
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
/// `image` 提供时在用户消息中附上该幻灯片截图（OpenAI 兼容 image_url 协议）。
fn chat(
    s: &LlmSettings,
    items: &[(usize, &str)],
    image: Option<&Path>,
) -> Result<Vec<(usize, String)>> {
    validate(s)?;
    let image_b64 = match image {
        Some(p) => {
            use base64::Engine as _;
            let bytes =
                std::fs::read(p).with_context(|| format!("读取幻灯片截图 {}", p.display()))?;
            Some(base64::engine::general_purpose::STANDARD.encode(bytes))
        }
        None => None,
    };
    let body = build_chat_body(s, items, image_b64.as_deref())?;
    let resp = ureq::post(&endpoint(&s.base_url))
        .timeout(Duration::from_secs(300))
        .set("Content-Type", "application/json")
        .set("Authorization", &format!("Bearer {}", s.api_key))
        .send_json(body)
        .context("LLM 请求失败")?;
    let v: serde_json::Value = resp.into_json().context("LLM 响应解析失败")?;
    let content = v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();
    parse_id_text_pairs(&content)
        .with_context(|| format!("LLM 响应不是 id/text JSON 数组: {:.200}", content))
}

/// 构造 /chat/completions 请求体（独立出来便于单测覆盖视觉路径）。
fn build_chat_body(
    s: &LlmSettings,
    items: &[(usize, &str)],
    image_b64: Option<&str>,
) -> Result<serde_json::Value> {
    let payload: Vec<serde_json::Value> = items
        .iter()
        .map(|(i, t)| serde_json::json!({"id": i, "text": t}))
        .collect();
    let vision_note = if image_b64.is_some() {
        " 消息附带该段对应的课件截图，仅用于校正术语拼写与专有名词，不要描述或评论图片本身。"
    } else {
        ""
    };
    let mut content = vec![serde_json::json!({
        "type": "text",
        "text": serde_json::to_string(&payload)?,
    })];
    if let Some(b64) = image_b64 {
        content.push(serde_json::json!({
            "type": "image_url",
            "image_url": {"url": format!("data:image/jpeg;base64,{b64}")},
        }));
    }
    Ok(serde_json::json!({
        "model": s.model,
        "temperature": 0.0,
        "max_tokens": 16384,
        "response_format": {"type": "json_object"},
        "messages": [
            {"role": "system", "content": format!("{} 输出为 JSON 数组，每项形如 {{\"id\":序号,\"text\":润色后的文本}}，id 必须与输入一一对应；纯语气词条目的 text 为空字符串。{vision_note}", effective_prompt(s))},
            {"role": "user", "content": content},
        ]
    }))
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
    let cleaned = clean_trailing_commas(slice);
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

fn clean_trailing_commas(s: &str) -> String {
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

/// 发原始 chat/completions 请求并返回 message.content（供 summarize 等复用）。
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
/// 使用 dialoguer（console 行编辑）：支持左右箭头/Home/End 移动、
/// 退格/删除等标准编辑键——裸 read_line 无法处理方向键转义序列（issue #3）。
pub fn setup_interactive(
    mut cfg: crate::settings::ConfigFile,
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    disable_hint: bool,
) -> Result<crate::settings::ConfigFile> {
    let cur_or = |v: String, cur: &str| {
        if v.trim().is_empty() {
            cur.to_string()
        } else {
            v
        }
    };

    if let Some(v) = base_url {
        cfg.llm.base_url = v;
    } else {
        let v: String = dialoguer::Input::new()
            .with_prompt("Base URL（OpenAI 兼容，如 https://api.deepseek.com/v1）")
            .with_initial_text(&cfg.llm.base_url)
            .allow_empty(true)
            .interact_text()?;
        cfg.llm.base_url = cur_or(v, &cfg.llm.base_url);
    }
    if let Some(v) = api_key {
        cfg.llm.api_key = v;
    } else {
        // 不回显已保存的 key：空输入 = 保留当前值
        let keep_hint = if cfg.llm.api_key.is_empty() {
            String::new()
        } else {
            format!(
                "（回车保留已配置的 {}...）",
                &cfg.llm.api_key[..cfg.llm.api_key.len().min(6)]
            )
        };
        let v: String = dialoguer::Input::<String>::new()
            .with_prompt(format!("API Key{keep_hint}"))
            .allow_empty(true)
            .interact_text()?;
        cfg.llm.api_key = cur_or(v, &cfg.llm.api_key);
    }
    if let Some(v) = model {
        cfg.llm.model = v;
    } else {
        let v: String = dialoguer::Input::new()
            .with_prompt("模型名（如 deepseek-chat）")
            .with_initial_text(&cfg.llm.model)
            .allow_empty(true)
            .interact_text()?;
        cfg.llm.model = cur_or(v, &cfg.llm.model);
    }
    // 容错：没写 scheme 时补 https://
    if !cfg.llm.base_url.is_empty() && !cfg.llm.base_url.contains("://") {
        cfg.llm.base_url = format!("https://{}", cfg.llm.base_url.trim());
    }
    // 视觉能力仅交互式终端询问（脚本化调用全部传参时不阻塞）
    if unsafe { libc::isatty(2) } == 1 {
        cfg.llm.vision = dialoguer::Select::new()
            .with_prompt("该模型支持视觉输入吗？（开启后润色时附幻灯片截图，辅助纠正技术词汇）")
            .items([
                "不支持 / 不使用（默认，纯文本润色）",
                "支持（需多模态模型，如 Gemini / GPT-4o / Qwen-VL）",
            ])
            .default(if cfg.llm.vision { 1 } else { 0 })
            .interact_opt()?
            .is_some_and(|i| i == 1);
    }
    cfg.llm.disable_hint = disable_hint;
    cfg.llm.enabled = true;
    Ok(cfg)
}

pub fn print_status(cfg: &crate::settings::ConfigFile) {
    let s = &cfg.llm;
    println!("配置文件：{}", crate::settings::config_path().display());
    println!(
        "  LLM 润色：{}",
        if s.enabled { "已开启" } else { "已关闭" }
    );
    println!(
        "  base_url：{}",
        if s.base_url.is_empty() {
            "-"
        } else {
            &s.base_url
        }
    );
    let key_disp = if s.api_key.is_empty() {
        "-".to_string()
    } else {
        format!("{}...（已隐藏）", &s.api_key[..s.api_key.len().min(6)])
    };
    println!("  api_key ：{key_disp}");
    println!(
        "  model   ：{}",
        if s.model.is_empty() { "-" } else { &s.model }
    );
    println!(
        "  结束提示：{}",
        if s.disable_hint {
            "已关闭"
        } else {
            "开启"
        }
    );
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

    fn test_settings() -> LlmSettings {
        LlmSettings {
            enabled: true,
            base_url: "https://api.x.com/v1".into(),
            api_key: "k".into(),
            model: "m".into(),
            prompt: None,
            disable_hint: false,
            vision: false,
        }
    }

    #[test]
    fn apply_polish_empty_text_deletes_entry() {
        let mut chunk = vec![
            TranscriptEvent {
                start: 0.0,
                end: 1.0,
                text: "今天讲编译原理".into(),
                raw: None,
            },
            TranscriptEvent {
                start: 1.0,
                end: 2.0,
                text: "啊".into(),
                raw: None,
            },
        ];
        let bad = apply_polish(
            &mut chunk,
            &[(0, "今天讲编译原理".into()), (1, String::new())],
        );
        assert!(!bad);
        assert_eq!(chunk[1].text, "", "纯语气词被置空");
        assert_eq!(chunk[1].raw.as_deref(), Some("啊"), "原文进 raw 溯源");
        // 调用方语义：置空的条目随后被 retain 删除
        chunk.retain(|e| !e.text.trim().is_empty());
        assert_eq!(chunk.len(), 1);
    }

    #[test]
    fn apply_polish_rejects_mismatched_ids() {
        let mut chunk = vec![TranscriptEvent {
            start: 0.0,
            end: 1.0,
            text: "a".into(),
            raw: None,
        }];
        assert!(apply_polish(
            &mut chunk,
            &[(0, "x".into()), (1, "y".into())]
        ));
        assert!(apply_polish(&mut chunk, &[]));
        assert_eq!(chunk[0].text, "a", "不匹配时保留原文");
    }

    #[test]
    fn chat_body_text_vs_vision() {
        let s = test_settings();
        let items = [(0usize, "hello")];
        let text_only = build_chat_body(&s, &items, None).unwrap();
        let user: &Vec<serde_json::Value> = text_only["messages"][1]["content"].as_array().unwrap();
        assert_eq!(user.len(), 1);
        assert_eq!(user[0]["type"], "text");

        let vision = build_chat_body(&s, &items, Some("aGVsbG8=")).unwrap();
        let user: &Vec<serde_json::Value> = vision["messages"][1]["content"].as_array().unwrap();
        assert_eq!(user.len(), 2, "带图时附 image_url 内容块");
        assert_eq!(user[1]["type"], "image_url");
        assert_eq!(
            user[1]["image_url"]["url"].as_str().unwrap(),
            "data:image/jpeg;base64,aGVsbG8="
        );
        let sys = vision["messages"][0]["content"].as_str().unwrap();
        assert!(sys.contains("课件截图"), "带图时系统提示说明截图用途");
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
