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

pub async fn run(cfg: &PipelineConfig, input: AsrInput) -> Result<Vec<TranscriptEvent>> {
    let ngl = if cfg.provider.eq_ignore_ascii_case("cpu") {
        0
    } else {
        99
    };
    let threads = cfg.threads;
    let max_speech = cfg.max_speech;
    tokio::task::spawn_blocking(move || run_blocking(&input, ngl, threads, max_speech))
        .await
        .context("ASR 线程 join 失败")?
}

fn run_blocking(
    input: &AsrInput,
    ngl: i32,
    threads: i32,
    max_speech: f32,
) -> Result<Vec<TranscriptEvent>> {
    let t0 = Instant::now();
    let segs = ffmpeg_vad(&input.wav, max_speech)?;
    tracing::info!(segs = segs.len(), "vad");
    if segs.is_empty() {
        anyhow::bail!("没有检测到语音");
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
    tracing::info!(secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()), "server ready");

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

    let mut events = vec![];
    let mut err: Option<anyhow::Error> = None;
    for (i, (start, end)) in segs.iter().copied().enumerate() {
        let chunk = tmp.join(format!("c{i:04}.wav"));
        if let Err(e) = cut_wav(&input.wav, start, end, &chunk) {
            err = Some(e);
            break;
        }
        match transcribe_file(&base, &chunk) {
            Ok(raw) => {
                let text = sanitize_qwen_text(&raw);
                if !text.is_empty() {
                    events.push(TranscriptEvent { start, end, text });
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
    tracing::info!(n = events.len(), secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()), "asr done");
    Ok(events)
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
        if ureq::get(&url).timeout(Duration::from_secs(2)).call().is_ok() {
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

fn ffmpeg_vad(wav: &Path, max_speech: f32) -> Result<Vec<(f64, f64)>> {
    let out = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-i",
        ])
        .arg(wav)
        .args(["-af", "silencedetect=noise=-28dB:d=0.4", "-f", "null", "-"])
        .output()
        .context("ffmpeg silencedetect")?;
    let log = String::from_utf8_lossy(&out.stderr);
    let dur = crate::media::probe_duration_blocking(wav).unwrap_or(0.0);
    let mut silences: Vec<(f64, f64)> = vec![];
    let mut start: Option<f64> = None;
    for line in log.lines() {
        if let Some(v) = line.split("silence_start:").nth(1) {
            start = v.trim().parse().ok();
        } else if let Some(v) = line.split("silence_end:").nth(1) {
            let end: f64 = v.split_whitespace().next().unwrap_or("").parse().unwrap_or(0.0);
            if let Some(s) = start.take() {
                silences.push((s, end));
            }
        }
    }
    let mut speech = invert_silence(dur, &silences);
    speech = split_max(speech, max_speech as f64);
    speech.retain(|(a, b)| b - a >= 0.2);
    if speech.is_empty() && dur > 0.2 {
        speech = split_max(vec![(0.0, dur)], max_speech as f64);
    }
    Ok(speech)
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

fn split_max(segs: Vec<(f64, f64)>, max: f64) -> Vec<(f64, f64)> {
    let mut out = vec![];
    for (a, b) in segs {
        let mut x = a;
        while b - x > max {
            out.push((x, x + max));
            x += max;
        }
        if b - x >= 0.2 {
            out.push((x, b));
        }
    }
    out
}

fn cut_wav(src: &Path, start: f64, end: f64, dest: &Path) -> Result<()> {
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
