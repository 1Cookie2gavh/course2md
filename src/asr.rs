//! 语音识别：ffmpeg 静音检测分段 + llama.cpp（Qwen3-ASR）。
//!
//! 通过 `llama-server` 常驻进程走 GPU（macOS Metal / NVIDIA CUDA / CPU），
//! 跨平台只依赖 PATH 上的 llama.cpp。

use crate::config::PipelineConfig;
use crate::timeline::TranscriptEvent;
use anyhow::{Context, Result};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub struct AsrInput {
    pub wav: PathBuf,
    pub model: PathBuf,
    pub mmproj: PathBuf,
}

/// 清洗 Qwen3 转写中的提示词残留。
pub fn sanitize_qwen_text(s: &str) -> String {
    let mut t = s.trim();
    if let Some(p) = t.find("</asr_text>") {
        t = t[..p].trim();
    }
    if let Some(p) = t.rfind("<asr_text>") {
        t = t[p + "<asr_text>".len()..].trim();
    }
    let lower = t.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("language ")
        && let Some(i) = rest.find(char::is_whitespace)
    {
        // 原串对齐
        let skip = "language ".len() + i + 1;
        if skip <= t.len() {
            t = t[skip..].trim();
        }
    }
    t.to_string()
}

pub async fn run(cfg: &PipelineConfig, wav: &std::path::Path) -> Result<Vec<TranscriptEvent>> {
    use crate::checkpoint::{AsrIdentity, Checkpoint};
    let provider = cfg.provider.to_ascii_lowercase();
    let open = |id: &AsrIdentity| Checkpoint::open(&cfg.out_dir, cfg.resume, id);

    if provider == "api" {
        let id = AsrIdentity::new("api", &cfg.asr_api.model, cfg.max_speech);
        let mut cp = open(&id)?;
        let api = cfg.asr_api.clone();
        let max_speech = cfg.max_speech as f64;
        let wav = wav.to_path_buf();
        let joined = tokio::task::spawn_blocking(move || {
            let r = run_api(&api, &wav, max_speech, &mut cp);
            if r.is_ok() {
                cp.finish()?;
            }
            r
        })
        .await
        .context("ASR 线程 join 失败")?;
        return joined;
    }
    if provider == "npu" {
        let model = crate::npu::resolve_npu_model(cfg.asr_model.as_deref());
        let id = AsrIdentity::new("npu", &model, cfg.max_speech);
        let mut cp = open(&id)?;
        let max_speech = cfg.max_speech as f64;
        let wav = wav.to_path_buf();
        let cfg = cfg.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let r = crate::npu::run_npu(&cfg, &wav, max_speech, &mut cp);
            if r.is_ok() {
                cp.finish()?;
            }
            r
        })
        .await
        .context("ASR 线程 join 失败")?;
        return joined;
    }
    if provider == "coreml" {
        #[cfg(apple_native)]
        {
            let wav = wav.to_path_buf();
            let max_speech = cfg.max_speech as f64;
            let model = crate::apple::resolve_model(
                cfg.asr_model.as_deref().filter(|s| !s.trim().is_empty()),
            );
            let id = AsrIdentity::new("coreml", &model, cfg.max_speech);
            let mut cp = open(&id)?;
            let joined = tokio::task::spawn_blocking(move || {
                let tmp =
                    std::env::temp_dir().join(format!("course2md-asr-{}", std::process::id()));
                let _ = std::fs::create_dir_all(&tmp);
                let res =
                    crate::apple::run_coreml(&wav, max_speech, &model, cut_wav, &tmp, &mut cp);
                let res = res.and_then(|ev| cp.finish().map(|()| ev));
                let _ = std::fs::remove_dir_all(&tmp);
                res
            })
            .await
            .context("ASR 线程 join 失败")?;
            match joined {
                Ok(events) => return Ok(events), // 空 = VAD 无语音（终态，不再回落）
                Err(e) => tracing::warn!("CoreML 后端失败（{e:#}），回落 llama-server"),
            }
        }
        #[cfg(not(apple_native))]
        {
            anyhow::bail!(
                "此构建未包含 Apple CoreML 后端（仅 macOS Apple Silicon 构建支持）。请用 --provider gpu 或 cpu"
            );
        }
    }
    let ngl = if provider == "cpu" { 0 } else { 99 };
    let threads = cfg.threads;
    let max_speech = cfg.max_speech;
    let llama = crate::models::ensure_llama_or_download(&cfg.model_dir).await?;
    // coreml 回落场景：身份随实际转写后端（llama/qwen3），旧 coreml 进度作废，
    // 避免同一 checkpoint 混入两个模型的转写文本。
    let id = AsrIdentity::new("llama", "qwen3-1.7b-gguf", cfg.max_speech);
    let mut cp = open(&id)?;
    let input = AsrInput {
        wav: wav.to_path_buf(),
        model: llama.model,
        mmproj: llama.mmproj,
    };
    tokio::task::spawn_blocking(move || {
        let r = run_blocking(&input, ngl, threads, max_speech, &mut cp);
        if r.is_ok() {
            cp.finish()?;
        }
        r
    })
    .await
    .context("ASR 线程 join 失败")?
}

fn run_blocking(
    input: &AsrInput,
    ngl: i32,
    threads: i32,
    max_speech: f32,
    cp: &mut crate::checkpoint::Checkpoint,
) -> Result<Vec<TranscriptEvent>> {
    let t0 = Instant::now();
    let segs = ffmpeg_vad(&input.wav, max_speech)?;
    tracing::info!(segs = segs.len(), "vad");
    if segs.is_empty() {
        tracing::warn!("未检测到语音（VAD 结果为空），跳过识别");
        return Ok(vec![]);
    }

    let bin = find_llama_server()?;
    let port = free_port()?;
    tracing::info!(bin = %bin.display(), port, ngl, "llama-server");
    let mut child = spawn_server(&bin, &input.model, &input.mmproj, ngl, threads, port)?;
    let base = format!("http://127.0.0.1:{port}");
    if let Err(e) = wait_ready(&base, Duration::from_secs(300)) {
        let _ = child.kill();
        return Err(e);
    }
    tracing::info!(
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "server ready"
    );

    let tmp = std::env::temp_dir().join(format!("course2md-asr-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let pb = indicatif::ProgressBar::new(segs.len() as u64);
    pb.set_style(
        indicatif::ProgressStyle::with_template(
            "{spinner:.green} asr {pos}/{len} [{bar:32.cyan/blue}] {elapsed} {msg}",
        )
        .unwrap()
        .progress_chars("##-"),
    );

    let mut err: Option<anyhow::Error> = None;
    for (i, seg) in segs.iter().copied().enumerate() {
        let (start, end) = (seg.start, seg.end);
        if cp.is_done(start, end) {
            pb.inc(1);
            continue; // 断点续跑：该 chunk 上次已完成
        }
        let chunk = tmp.join(format!("c{i:04}.wav"));
        if let Err(e) = cut_wav(&input.wav, seg.cut_start, seg.cut_end, &chunk) {
            err = Some(e);
            break;
        }
        match transcribe_file(&base, &chunk) {
            Ok(raw) => {
                let text = sanitize_qwen_text(&raw);
                // 空文本也记录（静音 chunk）；写盘失败则中断且不标记完成
                if let Err(e) = cp.record(start, end, &text) {
                    err = Some(e);
                    break;
                }
            }
            Err(e) => {
                err = Some(e);
                break;
            }
        }
        let _ = std::fs::remove_file(&chunk);
        pb.inc(1);
    }
    pb.finish_and_clear();
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&tmp);
    if let Some(e) = err {
        return Err(e);
    }
    // 事件统一来自 checkpoint（历史 + 本次），按时间排序
    let mut all: Vec<TranscriptEvent> = cp.events().to_vec();
    all.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
    tracing::info!(
        n = all.len(),
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "asr done"
    );
    Ok(all)
}

/// 云端 STT：ffmpeg VAD 分段 + 逐段 POST /audio/transcriptions（OpenAI 兼容 / OpenRouter）。
fn run_api(
    api: &crate::settings::AsrApi,
    wav: &Path,
    max_speech: f64,
    cp: &mut crate::checkpoint::Checkpoint,
) -> Result<Vec<TranscriptEvent>> {
    let t0 = Instant::now();
    // key 解析（非递归）：配置 > 非空环境变量；空值不覆盖（防无限递归）
    let api_key = if !api.api_key.trim().is_empty() {
        api.api_key.clone()
    } else {
        std::env::var("OPENROUTER_API_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty())
            .context("云端 STT 未配置 API Key：在配置文件 [asr_api] 设置 api_key，或用 --asr-api-key / OPENROUTER_API_KEY")?
    };
    let api = crate::settings::AsrApi {
        api_key,
        ..api.clone()
    };
    let api = &api;
    let segs = ffmpeg_vad(wav, max_speech as f32)?;
    tracing::info!(segs = segs.len(), endpoint = %api.base_url, model = %api.model, "api vad");
    if segs.is_empty() {
        tracing::warn!("未检测到语音（VAD 结果为空），跳过识别");
        return Ok(vec![]);
    }

    let tmp = std::env::temp_dir().join(format!("course2md-asr-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let url = format!(
        "{}/audio/transcriptions",
        api.base_url.trim().trim_end_matches('/')
    );
    let pb = indicatif::ProgressBar::new(segs.len() as u64);
    pb.set_style(
        indicatif::ProgressStyle::with_template(
            "{spinner:.green} asr {pos}/{len} [{bar:32.cyan/blue}] {elapsed} {msg}",
        )
        .unwrap()
        .progress_chars("##-"),
    );

    let client = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(120))
        .build();
    let client = std::sync::Arc::new(client);
    let segs = std::sync::Arc::new(segs);
    let tmp = std::sync::Arc::new(tmp);
    let wav_path = std::sync::Arc::new(wav.to_path_buf());
    let model = api.model.clone();
    let key = api.api_key.clone();
    // 断点续跑：预计算每个 chunk 是否已完成（worker 线程不能借用 cp）
    let skip: Vec<std::sync::Arc<std::sync::atomic::AtomicBool>> = segs
        .iter()
        .map(|s| {
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                cp.is_done(s.start, s.end),
            ))
        })
        .collect();

    // 有界并发（默认 4）：网络往返是主要瓶颈；结果按下标保序
    const WORKERS: usize = 4;
    let (tx, rx) = std::sync::mpsc::channel::<(usize, Result<Option<String>, String>)>();
    let next = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handles: Vec<_> = (0..WORKERS)
        .map(|_| {
            let (tx, client, segs, tmp, model, key, url, next, abort, wav_path, skip) = (
                tx.clone(),
                client.clone(),
                segs.clone(),
                tmp.clone(),
                model.clone(),
                key.clone(),
                url.clone(),
                next.clone(),
                abort.clone(),
                wav_path.clone(),
                skip.clone(),
            );
            std::thread::spawn(move || {
                loop {
                    if abort.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if i >= segs.len() {
                        break;
                    }
                    let seg = segs[i];
                    if skip[i].load(std::sync::atomic::Ordering::Relaxed) {
                        continue; // 断点续跑（主线程预计算）
                    }
                    let r = transcribe_api(
                        &client,
                        &url,
                        &model,
                        &key,
                        &tmp.join(format!("c{i:04}.wav")),
                        seg,
                        &wav_path,
                    );
                    if tx.send((i, r)).is_err() {
                        break;
                    }
                }
            })
        })
        .collect();
    drop(tx);

    let mut results: Vec<Option<Option<String>>> = vec![None; segs.len()];
    let mut err: Option<anyhow::Error> = None;
    for (i, r) in rx {
        match r {
            Ok(text) => {
                // 空结果（None）同样记录完成，避免静音 chunk 反复重跑
                if let Err(e) = cp.record(segs[i].start, segs[i].end, text.as_deref().unwrap_or(""))
                    && err.is_none()
                {
                    err = Some(e);
                    abort.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                results[i] = Some(text);
            }
            Err(e) => {
                if err.is_none() {
                    err = Some(anyhow::anyhow!("云端 STT 失败（chunk {i}）：{e}"));
                    abort.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
        pb.inc(1);
    }
    for h in handles {
        let _ = h.join();
    }
    pb.finish_and_clear();
    let _ = std::fs::remove_dir_all(tmp.as_ref());
    if let Some(e) = err {
        return Err(e);
    }
    // 事件统一来自 checkpoint（收集循环里已 record），按时间排序
    let mut all: Vec<TranscriptEvent> = cp.events().to_vec();
    all.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
    let events = all;
    tracing::info!(
        n = events.len(),
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "asr done"
    );
    Ok(events)
}

/// 转写单个 chunk；Ok(None) = 无语音内容。
fn transcribe_api(
    // base64 Engine trait
    client: &ureq::Agent,
    url: &str,
    model: &str,
    key: &str,
    chunk: &Path,
    seg: Seg,
    wav: &Path,
) -> Result<Option<String>, String> {
    use base64::Engine as _;
    cut_wav(wav, seg.cut_start, seg.cut_end, chunk).map_err(|e| format!("切分音频失败: {e:#}"))?;
    let bytes = std::fs::read(chunk).map_err(|e| format!("读取 chunk 失败: {e}"))?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let body = serde_json::json!({
        "model": model,
        "input_audio": {"data": b64, "format": "wav"},
    });
    let resp = client
        .post(url)
        .set("Authorization", &format!("Bearer {key}"))
        .send_json(body)
        .map_err(|e| format!("请求失败: {e}"))?;
    let v: serde_json::Value = resp.into_json().map_err(|e| format!("响应解析失败: {e}"))?;
    if let Some(e) = v
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
    {
        return Err(format!("API 报错: {e}"));
    }
    let text = v["text"].as_str().unwrap_or("").trim().to_string();
    let _ = std::fs::remove_file(chunk);
    Ok(if text.is_empty() { None } else { Some(text) })
}

fn find_llama_server() -> Result<PathBuf> {
    for name in ["llama-server", "llama-server.exe"] {
        if let Some(p) = which(name) {
            return Ok(p);
        }
    }
    anyhow::bail!("找不到 llama-server，请安装 llama.cpp 并加入 PATH")
}

fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|p| p.join(cmd))
        .find(|p| p.is_file())
}

fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn spawn_server(
    bin: &Path,
    model: &Path,
    mmproj: &Path,
    ngl: i32,
    threads: i32,
    port: u16,
) -> Result<Child> {
    let mut cmd = Command::new(bin);
    cmd.arg("-m")
        .arg(model)
        .arg("--mmproj")
        .arg(mmproj)
        .arg("-ngl")
        .arg(ngl.to_string())
        .arg("-c")
        .arg("4096")
        .arg("-n")
        .arg("256")
        .arg("-t")
        .arg(threads.to_string())
        .arg("--port")
        .arg(port.to_string())
        .arg("--host")
        .arg("127.0.0.1")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    cmd.spawn().context("启动 llama-server 失败")
}

fn wait_ready(base: &str, timeout: Duration) -> Result<()> {
    let t0 = Instant::now();
    let url = format!("{base}/health");
    loop {
        if t0.elapsed() > timeout {
            anyhow::bail!("llama-server 启动超时");
        }
        if ureq::get(&url)
            .timeout(Duration::from_secs(2))
            .call()
            .is_ok()
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn transcribe_file(base: &str, wav: &Path) -> Result<String> {
    let bytes = std::fs::read(wav)?;
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let body = serde_json::json!({
        "temperature": 0.0,
        "max_tokens": 256,
        "messages": [{
            "role": "user",
            "content": [
                {"type": "text", "text": "Transcribe the audio."},
                {"type": "input_audio", "input_audio": {"data": b64, "format": "wav"}}
            ]
        }]
    });
    let resp = ureq::post(&format!("{base}/v1/chat/completions"))
        .timeout(Duration::from_secs(180))
        .set("Content-Type", "application/json")
        .send_json(body)
        .context("llama-server 识别请求失败")?;
    let v: serde_json::Value = resp.into_json()?;
    let text = v["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();
    if text.is_empty() {
        anyhow::bail!("llama-server 返回空文本: {v}");
    }
    Ok(text)
}

pub(crate) fn ffmpeg_vad(wav: &Path, max_speech: f32) -> Result<Vec<Seg>> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-i"])
        .arg(wav)
        .args(["-af", "silencedetect=noise=-28dB:d=0.4", "-f", "null", "-"])
        .output()
        .context("ffmpeg silencedetect")?;
    if !out.status.success() {
        anyhow::bail!(
            "ffmpeg silencedetect 失败（{}）：{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .rev()
                .take(3)
                .collect::<Vec<_>>()
                .join(" | ")
        );
    }
    let log = String::from_utf8_lossy(&out.stderr);
    let dur = crate::media::probe_duration_blocking(wav).unwrap_or(0.0);
    let mut silences: Vec<(f64, f64)> = vec![];
    let mut start: Option<f64> = None;
    for line in log.lines() {
        if let Some(v) = line.split("silence_start:").nth(1) {
            start = v.trim().parse().ok();
        } else if let Some(v) = line.split("silence_end:").nth(1) {
            let end: f64 = v
                .split_whitespace()
                .next()
                .unwrap_or("")
                .parse()
                .unwrap_or(0.0);
            if let Some(s) = start.take() {
                silences.push((s, end));
            }
        }
    }
    normalize_segments(invert_silence(dur, &silences), max_speech as f64, wav)
}

/// 最终送入 ASR 的分段：`start/end` 是事件时间（用于时间线），`cut_*` 是切音频范围（含静音填充）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Seg {
    pub start: f64,
    pub end: f64,
    pub cut_start: f64,
    pub cut_end: f64,
}

/// 音频逐 100ms 的 RMS 能量（用于在目标切点附近找最近的静音最低点）。
pub struct Energy {
    hop: f64,
    rms: Vec<f32>,
}

impl Energy {
    pub fn load(wav: &Path) -> Result<Self> {
        // 直接解析 16k 单声道 s16 wav（extract_audio 的固定产物）
        let data = std::fs::read(wav).with_context(|| format!("读取音频失败 {}", wav.display()))?;
        let (body, _) =
            find_pcm_body(&data).ok_or_else(|| anyhow::anyhow!("无法解析 wav PCM 数据"))?;
        let mut samples = Vec::with_capacity(body.len() / 2);
        for c in body.as_chunks::<2>().0 {
            samples.push(i16::from_le_bytes(*c));
        }
        const HOP: usize = 1600; // 100ms @16k
        let mut rms = Vec::with_capacity(samples.len() / HOP + 1);
        for ch in samples.chunks(HOP) {
            let s: f64 = ch
                .iter()
                .map(|&v| (v as f64 / 32768.0).powi(2))
                .sum::<f64>()
                / ch.len() as f64;
            rms.push((s.sqrt()) as f32);
        }
        Ok(Self { hop: 0.1, rms })
    }

    /// [a,b]（秒）内能量最低的时刻；无数据时返回 None。
    fn quietest(&self, a: f64, b: f64) -> Option<f64> {
        let i0 = (a / self.hop).ceil() as usize;
        let i1 = (b / self.hop).floor() as usize;
        if i1 <= i0 || i0 >= self.rms.len() {
            return None;
        }
        let i1 = i1.min(self.rms.len() - 1);
        let (bi, bv) = self.rms[i0..=i1]
            .iter()
            .enumerate()
            .min_by(|x, y| x.1.partial_cmp(y.1).unwrap())?;
        if bv.is_nan() {
            return None;
        }
        Some((i0 + bi) as f64 * self.hop + self.hop / 2.0)
    }
}

/// 跳过 wav 头，返回 (PCM body, sample_rate)。
fn find_pcm_body(data: &[u8]) -> Option<(&[u8], u32)> {
    if data.len() < 44 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        return None;
    }
    let mut pos = 12;
    let mut rate = 0;
    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let size = u32::from_le_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
            as usize;
        match id {
            b"fmt " => {
                if pos + 8 + 16 <= data.len() {
                    rate = u32::from_le_bytes([
                        data[pos + 12],
                        data[pos + 13],
                        data[pos + 14],
                        data[pos + 15],
                    ]);
                }
            }
            b"data" => {
                let end = (pos + 8 + size).min(data.len());
                return Some((&data[pos + 8..end], rate));
            }
            _ => {}
        }
        pos += 8 + size + (size & 1); // chunk 对齐
    }
    None
}

const PAD: f64 = 0.25; // 切音频时向两侧静音各延展的秒数
const SPLIT_WINDOW: f64 = 3.0; // 在目标切点 ± 此窗口内寻找静音最低点
const MIN_PIECE: f64 = 1.0; // 硬切产生的最短片段

/// VAD 后处理：能量感知切分 + 静音填充。
/// - 超过 max_speech 的段在 [target-3s, target+3s] 窗口内选能量最低点切（避开词中切断）
/// - 切音频时向两侧静音各填充 0.25s（只进静音、不进相邻语音，故无重复文本）
pub fn normalize_segments(
    speech: Vec<(f64, f64)>,
    max_speech: f64,
    wav: &Path,
) -> Result<Vec<Seg>> {
    let energy = Energy::load(wav).ok();
    let dur = crate::media::probe_duration_blocking(wav).unwrap_or(0.0);
    // VAD 成功但无语音（或全被短段过滤）→ 空分段。
    // 不再把整段音频当语音兜底：静音课件会诱发 ASR 幻觉。
    let mut raw = speech;
    raw.retain(|(a, b)| b - a >= 0.2);
    if raw.is_empty() {
        return Ok(vec![]);
    }

    let mut pieces: Vec<(f64, f64)> = vec![];
    for &(s, e) in &raw {
        split_smart(s, e, max_speech, energy.as_ref(), &mut pieces);
    }

    // 填充：VAD 外边界向真实静音扩展 0.25s（限制来自相邻原始语音段，而非本段自身）；
    // max_speech 内部切点已在能量最低点，不额外填充（避免相邻 chunk 重复文本）。
    let segs = pieces
        .iter()
        .map(|&(s, e)| {
            let host_idx = raw
                .iter()
                .position(|&(rs, re)| s >= rs - 1e-6 && e <= re + 1e-6);
            // 该 piece 是否是其所在 raw 段的第一片/最后一片（外边界才能 pad）
            let (is_first_of_host, is_last_of_host) = match host_idx {
                Some(h) => {
                    let (rs, re) = raw[h];
                    let first = (s - rs).abs() < 1e-6;
                    let last = (re - e).abs() < 1e-6;
                    (first, last)
                }
                None => (true, true),
            };
            // 前一个原始语音段的终点（跨段静音的上限）
            let speech_lo = match host_idx {
                Some(h) if h > 0 => raw[h - 1].1,
                _ => 0.0,
            };
            let speech_hi = match host_idx {
                Some(h) if h + 1 < raw.len() => raw[h + 1].0,
                _ => dur,
            };
            let cut_start = if is_first_of_host {
                (s - PAD).max(speech_lo).max(0.0)
            } else {
                s
            };
            let cut_end = if is_last_of_host {
                let hi = if dur > 0.0 { speech_hi } else { e + PAD };
                (e + PAD).min(hi)
            } else {
                e
            };
            Seg {
                start: s,
                end: e,
                cut_start,
                cut_end: cut_end.max(e),
            }
        })
        .collect();
    Ok(segs)
}

/// 递归切分：优先在静音最低点切，找不到则回退硬切。
fn split_smart(s: f64, e: f64, max: f64, energy: Option<&Energy>, out: &mut Vec<(f64, f64)>) {
    if e - s <= max {
        out.push((s, e));
        return;
    }
    let target = s + max;
    // 只在 [target-3s, target] 内找静音最低点：任何 piece 都不超过 max（硬上限，
    // ASR 后端常有上下文长度限制）
    let w0 = (target - SPLIT_WINDOW).max(s + MIN_PIECE.min(max / 2.0));
    let w1 = target;
    let cut = energy
        .and_then(|en| en.quietest(w0.max(s), w1))
        .unwrap_or(target);
    let cut = cut.clamp(s + 0.5, target);
    out.push((s, cut));
    split_smart(cut, e, max, energy, out);
}

fn invert_silence(dur: f64, sil: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut t = 0.0;
    let mut out = vec![];
    for &(s, e) in sil {
        if s > t + 0.15 {
            out.push((t, s));
        }
        t = e.max(t);
    }
    if dur > t + 0.15 {
        out.push((t, dur));
    }
    out
}

pub fn cut_wav(src: &Path, start: f64, end: f64, dest: &Path) -> Result<()> {
    let dur = (end - start).max(0.05);
    let st = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-ss"])
        .arg(format!("{start:.3}"))
        .arg("-t")
        .arg(format!("{dur:.3}"))
        .arg("-i")
        .arg(src)
        .args(["-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(dest)
        .status()
        .context("ffmpeg cut")?;
    if !st.success() {
        anyhow::bail!("ffmpeg 切分失败");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_and_vad_invert() {
        assert_eq!(
            sanitize_qwen_text("**language Chinese<asr_text>你好世界。"),
            "你好世界。"
        );
        assert_eq!(sanitize_qwen_text("内容</asr_text>尾巴"), "内容");
        let s = invert_silence(10.0, &[(0.0, 1.0), (4.0, 5.0)]);
        assert!((s[0].0 - 1.0).abs() < 1e-6 && (s[0].1 - 4.0).abs() < 1e-6);
    }
}
