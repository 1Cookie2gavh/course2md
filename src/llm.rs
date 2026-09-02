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
use std::io::{IsTerminal, Write};
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LlmSettings {
    pub enabled: bool,
    /// OpenAI 兼容 base URL，如 https://api.deepseek.com/v1
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    /// 自定义校对指令（输出格式约束由系统自动追加，prompt 无法覆盖）
    pub prompt: Option<String>,
    /// 关闭「可开启 LLM」的结束提示
    pub disable_hint: bool,
    /// 视觉润色：每个请求附对应幻灯片截图，辅助纠正技术词汇（模型须支持图片输入）
    pub vision: bool,
    /// 转换完成后自动生成视频总结并写入 md/html（需 enabled）
    pub summarize: bool,
    /// 润色并发数（Section 间相互独立；自建网关/代理可调高）
    pub concurrency: usize,
}

impl Default for LlmSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: String::new(),
            api_key: String::new(),
            model: String::new(),
            prompt: None,
            disable_hint: false,
            vision: false,
            summarize: false,
            concurrency: DEFAULT_CONCURRENCY,
        }
    }
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

/// LLM 润色默认并发数（Section 间相互独立；可经 [llm] concurrency 调整）。
const DEFAULT_CONCURRENCY: usize = 8;
/// LLM 请求最大尝试次数（1 次原始 + 重试）。
const MAX_ATTEMPTS: usize = 3;

/// chat/completions 公共参数：温度固定 0（校对/总结都要求确定性输出）。
pub(crate) const CHAT_TEMPERATURE: f64 = 0.0;
/// 单次请求输出 token 上限（润色与总结共用）。
pub(crate) const CHAT_MAX_TOKENS: u32 = 16384;

/// 共享进度条样式：模板均为静态字符串，解析失败是编程错误。
pub(crate) fn progress_style(template: &str) -> indicatif::ProgressStyle {
    indicatif::ProgressStyle::with_template(template)
        .expect("静态进度条模板")
        .progress_chars("##-")
}

/// 构造标准 chat/completions 请求体（temperature=0、json_object 结构化输出）。
/// 润色与总结共用，避免两处各自拼 body 参数漂移；
/// `user` 传 &str 为纯文本消息，传 `serde_json::Value::Array` 为多模态内容块。
pub(crate) fn chat_body(
    model: &str,
    system: &str,
    user: impl Into<serde_json::Value>,
    max_tokens: u32,
) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "temperature": CHAT_TEMPERATURE,
        "max_tokens": max_tokens,
        "response_format": {"type": "json_object"},
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user.into()},
        ]
    })
}

/// 对已合并的 Section 做润色（在 merge 之后调用）。
/// - 失败批次保留原文（润色失败不阻断转换）
/// - vision=true 且截图存在时，请求附该节幻灯片；带图失败自动降级纯文本重试一次
/// - 模型对纯语气词条目返回空 text → 该条被删除（issue #5）
/// - Section 间真并发（worker 池抢占式取活，无波次队头阻塞）；
///   批次失败拆半递归重试 + 请求级指数退避，推理模型下更稳更快
pub fn polish_sections(sections: &mut [Section], frames_root: &Path, s: &LlmSettings) {
    let total: usize = sections
        .iter()
        .map(|sec| sec.speech.chunks(BATCH).len())
        .sum();
    let pb = indicatif::ProgressBar::new(total as u64);
    pb.set_style(progress_style(
        "{spinner:.green} llm {pos}/{len} [{bar:32.cyan/blue}] {msg}",
    ));
    let warned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let vision_warned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let workers = s.concurrency.clamp(1, 16);
    // worker 池：共享迭代器抢占式取 Section，谁先完成谁取下一个
    // （旧波次实现里最慢的 Section 会挡住整队）
    let queue = std::sync::Mutex::new(sections.iter_mut());
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let next = queue.lock().map(|mut it| it.next());
                    match next {
                        Ok(Some(sec)) => {
                            polish_section(s, frames_root, sec, &pb, &warned, &vision_warned);
                        }
                        Ok(None) => break,
                        Err(_) => break, // 中毒锁：其余 worker 会同样退出
                    }
                }
            });
        }
    });
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
    // 同一 Section 的多 chunk 共用一张截图：只读盘 + base64 一次
    //（数 MB 大，逐 chunk 重复编码太贵）；读取失败则该节按纯文本润色
    let image_b64 = if s.vision {
        let p = frames_root.join(&sec.image);
        if p.is_file() {
            match std::fs::read(&p) {
                Ok(bytes) => {
                    use base64::Engine as _;
                    Some(base64::engine::general_purpose::STANDARD.encode(bytes))
                }
                Err(e) => {
                    warn_once(
                        warned,
                        &format!("读取幻灯片截图 {} 失败（{e:#}），该节按纯文本润色", p.display()),
                    );
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };
    for chunk in sec.speech.chunks_mut(BATCH) {
        pb.inc(1);
        polish_chunk(s, chunk, image_b64.as_deref(), warned, vision_warned);
    }
    // 删除「成功润色为空串」的纯语气词条目
    sec.speech.retain(|e| !e.text.trim().is_empty());
}

/// 递归润色一个分块；失败（含 id 集不匹配）时拆半重试，保证尽力而为。
/// `image_b64` 为该节幻灯片截图的 base64（由 polish_section 统一读取一次）。
fn polish_chunk(
    s: &LlmSettings,
    chunk: &mut [TranscriptEvent],
    image_b64: Option<&str>,
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
    let r = match (chat(s, &items, image_b64), image_b64) {
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
                    split_and_retry(s, chunk, image_b64, warned, vision_warned);
                } else {
                    warn_once(warned, "LLM 返回 id 集与输入不符，该批保留原文");
                }
            }
        }
        Err(e) => {
            if chunk.len() > 1 {
                split_and_retry(s, chunk, image_b64, warned, vision_warned);
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
    image_b64: Option<&str>,
    warned: &std::sync::atomic::AtomicBool,
    vision_warned: &std::sync::atomic::AtomicBool,
) {
    let mid = chunk.len() / 2;
    polish_chunk(s, &mut chunk[..mid], image_b64, warned, vision_warned);
    polish_chunk(s, &mut chunk[mid..], image_b64, warned, vision_warned);
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
/// `image_b64` 提供时在用户消息中附上该幻灯片截图（OpenAI 兼容 image_url 协议）。
fn chat(
    s: &LlmSettings,
    items: &[(usize, &str)],
    image_b64: Option<&str>,
) -> Result<Vec<(usize, String)>> {
    validate(s)?;
    let body = build_chat_body(s, items, image_b64)?;
    let content = send_chat(s, &body)?;
    if let Some(pairs) = parse_id_text_pairs(&content) {
        return Ok(pairs);
    }
    // 偶发输出瑕疵（单个对象/未转义引号导致 JSON 非法）：用严格 JSON 指令重试一次
    let payload: Vec<serde_json::Value> = items
        .iter()
        .map(|(i, t)| serde_json::json!({"id": i, "text": t}))
        .collect();
    let repair_body = chat_body(
        &s.model,
        "你是严格的 JSON 输出器。输出必须且只能是一个 JSON 对象数组，每项形如 {\"id\":序号,\"text\":\"校对后的文本\"}。字符串内的双引号必须转义为 \\\"，或改用中文全角引号“”。禁止输出代码围栏、注释或任何数组之外的内容。",
        serde_json::to_string(&payload)?,
        CHAT_MAX_TOKENS,
    );
    let content2 = send_chat(s, &repair_body)?;
    parse_id_text_pairs(&content2)
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
    let system = format!("{} 输出为 JSON 数组，每项形如 {{\"id\":序号,\"text\":润色后的文本}}，id 必须与输入一一对应；纯语气词条目的 text 为空字符串。字符串内的双引号请用中文全角引号“”或转义为 \\\"，确保 JSON 合法。{vision_note}", effective_prompt(s));
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
    Ok(chat_body(
        &s.model,
        &system,
        serde_json::Value::Array(content),
        CHAT_MAX_TOKENS,
    ))
}

/// 从模型输出提取 [{"id":n,"text":"..."}]（容忍代码围栏、前后杂文、尾逗号与个别坏项，
/// 以及模型偶发的"单个对象"输出）。
pub fn parse_id_text_pairs(content: &str) -> Option<Vec<(usize, String)>> {
    if let Some(start) = content.find('[')
        && let Some(end) = content.rfind(']')
        && end > start
    {
        let slice = &content[start..=end];
        // 1) 严格解析；个别项 id/text 类型不对时 parse_items 返回 None，
        //    必须继续降级而不是整批丢弃（落入第 2/3 级）
        if let Ok(v) = serde_json::from_str::<Vec<serde_json::Value>>(slice)
            && let Some(items) = parse_items(&v)
        {
            return Some(items);
        }
        // 2) 清除尾逗号后重试
        let cleaned = clean_trailing_commas(slice);
        if cleaned != slice
            && let Ok(v) = serde_json::from_str::<Vec<serde_json::Value>>(&cleaned)
            && let Some(items) = parse_items(&v)
        {
            return Some(items);
        }
        // 3) 宽容扫描：跳过坏项，收集合法 {"id":..,"text":".."}
        if let Some(items) = lenient_scan(slice) {
            return Some(items);
        }
    }
    // 4) 模型偶发返回单个对象（非数组）：按单元素数组处理，交给调用方拆半重试
    parse_single_object(content)
}

/// 模型偶发输出 `{"id":0,"text":"..."}` 单个对象时兜底为单元素列表。
fn parse_single_object(content: &str) -> Option<Vec<(usize, String)>> {
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    if end <= start {
        return None;
    }
    let slice = &content[start..=end];
    let cleaned = clean_trailing_commas(slice);
    let v = serde_json::from_str::<serde_json::Value>(&cleaned).ok()?;
    let id = v.get("id")?.as_u64()? as usize;
    let text = v.get("text")?.as_str()?.to_string();
    Some(vec![(id, text)])
}

fn parse_items(v: &[serde_json::Value]) -> Option<Vec<(usize, String)>> {
    let mut out = vec![];
    for item in v {
        let id = item.get("id")?.as_u64()? as usize;
        let text = item.get("text")?.as_str()?.to_string();
        out.push((id, text));
    }
    if out.is_empty() { None } else { Some(out) }
}

pub(crate) fn clean_trailing_commas(s: &str) -> String {
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

/// 逐个扫描顶层 {...} 对象，坏项跳过；能取到至少一项即返回。
/// 按对象顺序遍历（此前实现从 "id" 向后找 {，方向反了，会漏掉首对象）。
fn lenient_scan(s: &str) -> Option<Vec<(usize, String)>> {
    let bytes = s.as_bytes();
    let mut out: Vec<(usize, String)> = vec![];
    let mut i = 0usize;
    let mut guard = 0usize;
    while i < s.len() {
        let Some(rel) = s[i..].find('{') else { break };
        let obj_start = i + rel;
        // 找配对的 }（跳过字符串字面量内的花括号）
        let mut depth = 0usize;
        let mut in_str = false;
        let mut esc = false;
        let mut end = None;
        for (k, &b) in bytes[obj_start..].iter().enumerate() {
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
        let Some(end) = end else { break };
        guard += 1;
        if guard > 10_000 {
            break;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s[obj_start..end])
            && let (Some(id), Some(text)) = (
                v.get("id").and_then(|x| x.as_u64()).map(|x| x as usize),
                v.get("text")
                    .and_then(|x| x.as_str())
                    .map(|t| t.to_string()),
            )
        {
            out.push((id, text));
        }
        i = end;
    }
    if out.is_empty() { None } else { Some(out) }
}

/// 发原始 chat/completions 请求并返回 message.content（润色与总结共用）。
///
/// 兼容性降级：部分 OpenAI 兼容端点不支持 `response_format: json_object`
///（直接 400）。仅当错误是参数类 4xx 时去掉该字段重试一次。
pub(crate) fn send_chat(s: &LlmSettings, body: &serde_json::Value) -> Result<String> {
    let resp = match request_chat(s, body) {
        Ok(r) => r,
        Err(first) => {
            // 只有 400（或其他非 401/429 的 4xx）才有理由怀疑是 response_format
            // 不兼容；401（鉴权）/429（限流）/超时/5xx 与该字段无关，原样报错。
            let degradable = match first.status {
                Some(400) => true,
                Some(c) => (400..500).contains(&c) && c != 401 && c != 429,
                None => false,
            };
            if !(degradable && body.get("response_format").is_some()) {
                return Err(first.err);
            }
            let mut relaxed = body.clone();
            if let Some(obj) = relaxed.as_object_mut() {
                obj.remove("response_format");
            }
            // 降级请求只试一次：原请求已按 MAX_ATTEMPTS 重试过，这里只验证
            // response_format 兼容性，再走完整重试循环会成倍放大等待时间。
            match request_chat_once(s, &relaxed) {
                Ok(r) => {
                    tracing::debug!("端点不支持 response_format，降级重试成功");
                    r
                }
                Err(_) => return Err(first.err),
            }
        }
    };
    let v: serde_json::Value = resp.into_json().context("LLM 响应解析失败")?;
    Ok(v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string())
}

/// LLM 请求失败：携带 HTTP 状态码（网络错误为 None）与是否可重试，
/// 供 send_chat 判断 response_format 降级是否有意义。
struct ChatFailure {
    status: Option<u16>,
    retryable: bool,
    err: anyhow::Error,
}

/// 网络层错误 / 429 / 5xx 可重试；其余 4xx（鉴权、参数）重试无意义。
fn is_retryable(e: &ureq::Error) -> bool {
    match e {
        ureq::Error::Status(code, _) => *code == 429 || *code >= 500,
        ureq::Error::Transport(_) => true,
    }
}

/// 进程级抖动序列：与纳秒异或打散，避免并发请求同步重试（不引入 rand）。
static JITTER_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 第 attempt 次失败后的退避时长：1s、2s 指数增长 + 亚秒级抖动。
fn backoff_duration(attempt: usize) -> Duration {
    let base = 1_u64 << (attempt.saturating_sub(1).min(6)); // 1, 2, 4, ...
    let seq = JITTER_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()))
        .unwrap_or(0);
    let jitter_ns = (nanos ^ seq.wrapping_mul(0x9E37_79B9_7F4A_7C15)) % 500_000_000; // 0~500ms 抖动
    Duration::from_nanos(base * 1_000_000_000 + jitter_ns)
}

/// 单次请求（不重试）；状态错误带上服务端返回体（鉴权/限流/参数问题一目了然）。
fn request_chat_once(
    s: &LlmSettings,
    body: &serde_json::Value,
) -> std::result::Result<ureq::Response, ChatFailure> {
    match ureq::post(&endpoint(&s.base_url))
        .timeout(Duration::from_secs(300))
        .set("Content-Type", "application/json")
        .set("Authorization", &format!("Bearer {}", s.api_key))
        // &Value 实现了 Serialize：传引用，避免克隆含 base64 截图的数 MB 请求体
        .send_json(body)
    {
        Ok(resp) => Ok(resp),
        Err(e) => {
            let retryable = is_retryable(&e);
            let (status, err) = match e {
                ureq::Error::Status(code, resp) => {
                    let tail = resp.into_string().unwrap_or_default();
                    (
                        Some(code),
                        anyhow::anyhow!(
                            "LLM 端点返回 {code}：{}",
                            tail.chars().take(300).collect::<String>()
                        ),
                    )
                }
                other => (None, anyhow::anyhow!("LLM 请求失败: {other}")),
            };
            Err(ChatFailure {
                status,
                retryable,
                err,
            })
        }
    }
}

/// 发请求：可重试错误按指数退避重试，总共最多 [`MAX_ATTEMPTS`] 次尝试。
fn request_chat(
    s: &LlmSettings,
    body: &serde_json::Value,
) -> std::result::Result<ureq::Response, ChatFailure> {
    let mut last_err: Option<ChatFailure> = None;
    for attempt in 1..=MAX_ATTEMPTS {
        if attempt > 1 {
            let wait = backoff_duration(attempt - 1);
            tracing::warn!(
                attempt,
                of = MAX_ATTEMPTS,
                ?wait,
                "LLM 请求失败，指数退避后重试"
            );
            std::thread::sleep(wait);
        }
        match request_chat_once(s, body) {
            Ok(resp) => return Ok(resp),
            Err(f) if f.retryable => last_err = Some(f),
            Err(f) => return Err(f),
        }
    }
    Err(last_err.unwrap_or_else(|| ChatFailure {
        status: None,
        retryable: false,
        err: anyhow::anyhow!("LLM 请求失败"),
    }))
}

/// 用户自定义校对指令；空白视为未设置，回落到内置提示词。
/// 注意：输出格式约束（JSON 数组 / id 对应）由系统在构造请求体时自动追加，
/// 自定义 prompt 无法覆盖（见 build_chat_body）。
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
                cfg.llm.api_key.chars().take(6).collect::<String>()
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
    // 视觉能力仅交互式终端询问（脚本化调用全部传参时不阻塞）；
    // 检查 stdin 而非 stderr，与 dialoguer 读取的流一致
    if std::io::stdin().is_terminal() {
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
        format!("{}...（已隐藏）", s.api_key.chars().take(6).collect::<String>())
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
    let msg = format!(
        "\n提示：可用 LLM 自动润色字幕（修正语气词与明显识别错误），运行 `course2md llm setup` 一键开启。\n配置文件：{}（加 --no-llm-hint 或在配置中设 disable_hint 可关闭本提示）\n",
        path.display()
    );
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
            summarize: false,
            concurrency: 8,
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
    fn parse_pairs_tolerates_trailing_commas_and_bad_items() {
        // 尾逗号（推理模型常见输出）
        let got = parse_id_text_pairs("[{\"id\":0,\"text\":\"a\",},{\"id\":1,\"text\":\"b\",},]")
            .unwrap();
        assert_eq!(got, vec![(0, "a".into()), (1, "b".into())]);
        // 个别坏项：跳过而不丢弃整批
        let got = parse_id_text_pairs(
            "[{\"id\":0,\"text\":\"a\"},{\"id\":\"oops\"},{\"id\":2,\"text\":\"c\"}]",
        )
        .unwrap();
        assert_eq!(
            got,
            vec![(0, "a".into()), (2, "c".into())],
            "坏项应被跳过（随后的拆半重试会覆盖 id=1）"
        );
    }

    #[test]
    fn retry_classification_and_backoff() {
        use ureq::Error;
        // Transport 无公共构造器：经由真实失败请求构造（127.0.0.1:1 必连接拒绝）
        let transport = ureq::get("http://127.0.0.1:1/health")
            .timeout(Duration::from_millis(500))
            .call()
            .unwrap_err();
        assert!(
            matches!(transport, Error::Transport(_)),
            "closed port should be transport error"
        );
        assert!(is_retryable(&transport), "网络/TLS 错误可重试");
        let mk_status = |code: u16| {
            let resp = ureq::Response::new(code, "x", "").unwrap();
            Error::Status(code, resp)
        };
        assert!(is_retryable(&mk_status(429)), "限流可重试");
        assert!(is_retryable(&mk_status(500)));
        assert!(is_retryable(&mk_status(503)));
        assert!(!is_retryable(&mk_status(400)), "参数错误重试无意义");
        assert!(!is_retryable(&mk_status(401)), "鉴权错误重试无意义");
        // 指数退避：1s、2s、4s…（含 0~500ms 抖动）
        let b1 = backoff_duration(1).as_secs_f64();
        let b2 = backoff_duration(2).as_secs_f64();
        let b3 = backoff_duration(3).as_secs_f64();
        assert!((1.0..1.5).contains(&b1));
        assert!((2.0..2.5).contains(&b2));
        assert!((4.0..4.5).contains(&b3));
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

    #[test]
    fn parse_id_pairs_tolerates_single_object() {
        // 模型偶发返回单个对象而非数组：应兜底为单元素列表（交给拆半重试）
        let got =
            parse_id_text_pairs("{\"id\":0,\"text\":\"有人问我说：“古人没相机。”\"}").unwrap();
        assert_eq!(got, vec![(0, "有人问我说：“古人没相机。”".into())]);
    }

    #[test]
    fn parse_id_pairs_malformed_unescaped_quotes_returns_none() {
        // 未转义引号导致 JSON 非法：返回 None（由 chat 的严格重试兜底），不 panic
        assert!(
            parse_id_text_pairs("{\"id\":0,\"text\":\"有人问我说：\"古人没相机。\"\"}").is_none()
        );
    }
}
