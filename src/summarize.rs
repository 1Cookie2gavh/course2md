//! LLM 视频总结：基于带时间戳字幕生成 TL;DR / 核心要点 / 内容大纲。
//!
//! 支持超长视频：字幕超过阈值时自动 map-reduce（分段总结 → 合并）。
//! 幻觉防护：仅以字幕为输入、temperature=0、json_object 结构化输出、要点带时间戳可溯源。

use crate::fetch::VideoMeta;
use crate::llm::{self, LlmSettings};
use crate::timeline::TranscriptEvent;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// 直接单次总结的最大字幕字符数（约 4 万字符 ≈ 5-6 万 token，128K 上下文内安全）。
const DIRECT_CHAR_LIMIT: usize = 40_000;
/// map-reduce 每个分块的字符上限。
const CHUNK_CHAR_LIMIT: usize = 25_000;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OutlineItem {
    /// 章节起始秒数（绝对时间）
    pub t: f64,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Summary {
    pub tldr: String,
    pub key_points: Vec<String>,
    pub outline: Vec<OutlineItem>,
}

const SYSTEM_PROMPT: &str = "你是视频内容总结助手。根据提供的带时间戳字幕为视频生成结构化总结。\
严格要求：1) 只依据字幕内容，严禁编造字幕中不存在的事实、数字、人名或观点；\
2) 对不确定的信息宁可省略也不要猜测；3) 使用视频原语言输出；\
4) 只输出一个合法 JSON 对象，不要代码围栏、不要任何多余文字。";

fn build_transcript(events: &[TranscriptEvent]) -> String {
    let mut out = String::new();
    for e in events {
        out.push_str(&format!("[{}] {}\n", crate::render::fmt_ts(e.start), e.text));
    }
    out
}

fn user_prompt(transcript: &str) -> String {
    format!(
        "以下是视频字幕（每行 [mm:ss] 为起始时间）：\n\n{transcript}\n\n\
请输出 JSON 对象：{{\"tldr\": \"不超过120字的一句话概述\", \
\"key_points\": [3-6条要点，每条不超过60字], \
\"outline\": [{{\"t\": 起始秒数(数字), \"title\": \"章节标题\", \"detail\": \"该章节内容简述，不超过100字\"}}]}}。\
outline 按时间顺序覆盖整个视频，3-8 节。"
    )
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

fn parse_time(v: Option<&serde_json::Value>) -> f64 {
    if let Some(n) = v.and_then(|x| x.as_f64()) {
        return n;
    }
    if let Some(s) = v.and_then(|x| x.as_str()) {
        let s = s.trim().trim_start_matches('[').trim_end_matches(']');
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() == 2
            && let (Ok(m), Ok(sec)) = (parts[0].parse::<f64>(), parts[1].parse::<f64>())
        {
            return m * 60.0 + sec;
        }
        if parts.len() == 3
            && let (Ok(h), Ok(m), Ok(sec)) = (
                parts[0].parse::<f64>(),
                parts[1].parse::<f64>(),
                parts[2].parse::<f64>(),
            )
        {
            return h * 3600.0 + m * 60.0 + sec;
        }
    }
    0.0
}

fn parse_summary(content: &str) -> Option<Summary> {
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    if end <= start {
        return None;
    }
    let slice = &content[start..=end];
    let parsed = serde_json::from_str::<serde_json::Value>(slice)
        .or_else(|_| serde_json::from_str::<serde_json::Value>(&clean_trailing_commas(slice)))
        .ok()?;
    let tldr = parsed.get("tldr").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
    let mut key_points = vec![];
    if let Some(arr) = parsed.get("key_points").and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(s) = v.as_str() {
                let s = s.trim();
                if !s.is_empty() {
                    key_points.push(s.to_string());
                }
            }
        }
    }
    let mut outline = vec![];
    if let Some(arr) = parsed.get("outline").and_then(|v| v.as_array()) {
        for v in arr {
            let title = v.get("title").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            let detail = v.get("detail").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            let t = parse_time(v.get("t"));
            if !title.is_empty() || !detail.is_empty() {
                outline.push(OutlineItem { t, title, detail });
            }
        }
    }
    if tldr.is_empty() && key_points.is_empty() && outline.is_empty() {
        return None;
    }
    Some(Summary { tldr, key_points, outline })
}

fn chat_once(s: &LlmSettings, sys: &str, user: &str) -> Result<String> {
    let body = serde_json::json!({
        "model": s.model,
        "temperature": 0.0,
        "max_tokens": 16384,
        "response_format": {"type": "json_object"},
        "messages": [
            {"role": "system", "content": sys},
            {"role": "user", "content": user},
        ],
    });
    llm::send_chat(s, &body).context("LLM 总结请求失败")
}

/// 单次总结；解析失败带修复指令重试一次。
fn summarize_text(s: &LlmSettings, transcript: &str) -> Result<Summary> {
    let content = chat_once(s, SYSTEM_PROMPT, &user_prompt(transcript))?;
    if let Some(sm) = parse_summary(&content) {
        return Ok(sm);
    }
    let repair = chat_once(
        s,
        "你是严格的 JSON 输出器。输出必须且只能是一个合法 JSON 对象，包含 tldr、key_points、outline 字段；不要代码围栏、不要注释、不要多余文字。",
        &user_prompt(transcript),
    )?;
    parse_summary(&repair).with_context(|| format!("LLM 总结响应解析失败: {:.200}", content))
}

fn split_chunks(events: &[TranscriptEvent], char_limit: usize) -> Vec<Vec<TranscriptEvent>> {
    let mut chunks: Vec<Vec<TranscriptEvent>> = vec![];
    let mut cur: Vec<TranscriptEvent> = vec![];
    let mut cur_chars = 0usize;
    for e in events {
        let c = e.text.chars().count() + 16;
        if !cur.is_empty() && cur_chars + c > char_limit {
            chunks.push(std::mem::take(&mut cur));
            cur_chars = 0;
        }
        cur.push(e.clone());
        cur_chars += c;
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    if chunks.is_empty() {
        chunks.push(vec![]);
    }
    chunks
}

/// 主入口：对全部字幕生成总结；超长自动 map-reduce。
pub async fn summarize(
    s: &LlmSettings,
    events: &[TranscriptEvent],
    _meta: &VideoMeta,
) -> Result<Summary> {
    llm::validate(s)?;
    let total_chars: usize = events.iter().map(|e| e.text.chars().count() + 16).sum();
    let transcript = build_transcript(events);
    if total_chars <= DIRECT_CHAR_LIMIT {
        let t = transcript.clone();
        let s2 = s.clone();
        return tokio::task::spawn_blocking(move || summarize_text(&s2, &t))
            .await
            .context("总结线程 join 失败")?;
    }
    // ---- map-reduce ----
    let chunks = split_chunks(events, CHUNK_CHAR_LIMIT);
    tracing::info!(chunks = chunks.len(), chars = total_chars, "summary map-reduce");
    let mut partials: Vec<Summary> = Vec::new();
    for (idx, chunk) in chunks.iter().enumerate() {
        let t = build_transcript(chunk);
        let s2 = s.clone();
        let sm = tokio::task::spawn_blocking(move || summarize_text(&s2, &t))
            .await
            .context("总结线程 join 失败")?
            .unwrap_or_else(|e| {
                tracing::warn!("分段总结失败（chunk {idx}）：{e:#}");
                Summary { tldr: String::new(), key_points: vec![], outline: vec![] }
            });
        partials.push(sm);
    }
    // 合并分段总结
    let mut combiner_input = String::new();
    for (idx, sm) in partials.iter().enumerate() {
        combiner_input.push_str(&format!("== 第 {} 段总结 ==\n", idx + 1));
        if !sm.tldr.is_empty() {
            combiner_input.push_str(&format!("概述：{}\n", sm.tldr));
        }
        for p in &sm.key_points {
            combiner_input.push_str(&format!("- {p}\n"));
        }
        for o in &sm.outline {
            combiner_input.push_str(&format!("- [{:.0}s] {}：{}\n", o.t, o.title, o.detail));
        }
        combiner_input.push('\n');
    }
    let input = combiner_input.clone();
    let s2 = s.clone();
    let combined = tokio::task::spawn_blocking(move || {
        chat_once(
            &s2,
            SYSTEM_PROMPT,
            &format!(
                "以下是各分段的总结（时间已按原视频绝对秒数标注）：\n\n{input}\n\n\
请合并为整个视频的最终总结，输出 JSON：{{\"tldr\": \"不超过150字的一句话概述\", \
\"key_points\": [整个视频的3-8条要点], \
\"outline\": [{{\"t\":秒,\"title\":\"章节标题\",\"detail\":\"简述\"}}]}}"
            ),
        )
    })
    .await
    .context("合并线程 join 失败")?;
    if let Ok(combined) = combined.as_deref()
        && let Some(sm) = parse_summary(combined)
    {
        return Ok(sm);
    }
    // 合并失败：拼接分块总结兜底
    let mut tldr = String::new();
    let mut kp: Vec<String> = vec![];
    let mut ol: Vec<OutlineItem> = vec![];
    for sm in &partials {
        if tldr.is_empty() && !sm.tldr.is_empty() {
            tldr = sm.tldr.clone();
        }
        kp.extend(sm.key_points.iter().cloned());
        ol.extend(sm.outline.iter().cloned());
    }
    if kp.is_empty() && ol.is_empty() {
        bail!("视频总结失败：所有分段均未返回有效内容");
    }
    Ok(Summary { tldr, key_points: kp, outline: ol })
}

/// 生成插入 course.md 的总结区块（markdown）。
pub fn render_md_block(sm: &Summary) -> String {
    let mut out = String::from("\n## 📝 视频总结\n\n");
    out.push_str(&format!("> {}\n", sm.tldr));
    if !sm.key_points.is_empty() {
        out.push_str("\n### 核心要点\n\n");
        for p in &sm.key_points {
            out.push_str(&format!("- {p}\n"));
        }
    }
    if !sm.outline.is_empty() {
        out.push_str("\n### 内容大纲\n\n");
        for o in &sm.outline {
            out.push_str(&format!("- **{}** {}：{}\n", crate::render::fmt_ts(o.t), o.title, o.detail));
        }
    }
    out.push('\n');
    out
}

/// 生成插入 course.html 的总结区块（HTML）。
pub fn render_html_block(sm: &Summary) -> String {
    let mut out = String::new();
    out.push_str("<section class=\"summary\"><h2>📝 视频总结</h2>");
    out.push_str(&format!("<p class=\"mute\">{}</p>", crate::render::esc(&sm.tldr)));
    if !sm.key_points.is_empty() {
        out.push_str("<h3>核心要点</h3><ul>");
        for p in &sm.key_points {
            out.push_str(&format!("<li>{}</li>", crate::render::esc(p)));
        }
        out.push_str("</ul>");
    }
    if !sm.outline.is_empty() {
        out.push_str("<h3>内容大纲</h3><ul>");
        for o in &sm.outline {
            out.push_str(&format!(
                "<li><b>{}</b> {}：{}</li>",
                crate::render::esc(&crate::render::fmt_ts(o.t)),
                crate::render::esc(&o.title),
                crate::render::esc(&o.detail)
            ));
        }
        out.push_str("</ul>");
    }
    out.push_str("</section>");
    out
}

/// 把总结区块插入已渲染的 markdown（元信息之后、首个字幕小节之前）。
pub fn insert_into_md(md: &str, sm: &Summary) -> String {
    let block = render_md_block(sm);
    if let Some(pos) = md.find("\n## [") {
        let mut out = md.to_string();
        out.insert_str(pos, &block);
        return out;
    }
    let mut out = md.to_string();
    out.push_str(&block);
    out
}

/// 把总结区块插入已渲染的 HTML（</header> 之后）。
pub fn insert_into_html(html: &str, sm: &Summary) -> String {
    let block = render_html_block(sm);
    if let Some(pos) = html.find("</header>") {
        let insert_at = pos + "</header>".len();
        let mut out = html.to_string();
        out.insert_str(insert_at, &block);
        return out;
    }
    if let Some(pos) = html.rfind("</body>") {
        let mut out = html.to_string();
        out.insert_str(pos, &block);
        return out;
    }
    let mut out = html.to_string();
    out.push_str(&block);
    out
}

/// 生成独立总结文件（markdown），用于 -o 导出。
pub fn render_standalone_md(title: &str, sm: &Summary) -> String {
    let mut out = format!("# {title}\n\n");
    out.push_str(&render_md_block(sm));
    out
}

/// 把文件名中的非法字符替换为下划线（Windows 保留字符 + 全角引号等）。
pub fn sanitize_filename(name: &str) -> String {
    let mut s = String::new();
    for ch in name.chars() {
        match ch {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '\u{201c}' | '\u{201d}' | '\u{ff1f}' | '\u{ff1a}' => {
                s.push('_')
            }
            c if c.is_control() => s.push('_'),
            c => s.push(c),
        }
    }
    let s = s.trim().trim_end_matches('.').to_string();
    if s.is_empty() {
        "summary".to_string()
    } else {
        s
    }
}

/// 判断已渲染文档是否已包含总结区块（用于幂等跳过）。
pub fn contains_summary(md: &str) -> bool {
    md.contains("视频总结")
}

/// 从 markdown 中移除已有总结区块（--force 重写时使用）。
pub fn strip_md_summary(md: &str) -> String {
    let marker = "## 📝 视频总结";
    if let Some(start) = md.find(marker) {
        let start_at = md[..start].rfind('\n').map(|i| i + 1).unwrap_or(start);
        let after = &md[start..];
        let end = after.find("\n## [").map(|i| start + i).unwrap_or(md.len());
        let mut out = md[..start_at].to_string();
        out.push_str(&md[end..]);
        return out;
    }
    md.to_string()
}

/// 从 HTML 中移除已有总结区块（--force 重写时使用）。
pub fn strip_html_summary(html: &str) -> String {
    if let Some(start) = html.find("<section class=\"summary\">")
        && let Some(end_rel) = html[start..].find("</section>")
    {
        let end = start + end_rel + "</section>".len();
        let mut out = html[..start].to_string();
        out.push_str(&html[end..]);
        return out;
    }
    html.to_string()
}
