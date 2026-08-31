//! ASR 阶段：Silero VAD 切分语音段 → Qwen3-ASR 逐段离线解码。
//!
//! sherpa-onnx 的类型虽标注了 Send，但推理本身是 CPU 密集的同步 C 调用：
//! 整个阶段在单个阻塞线程内完成（构造 → VAD → 循环解码 → 返回事件），
//! 对 tokio 而言就是一个可 await 的阻塞任务。

use crate::config::PipelineConfig;
use crate::models::Qwen3Paths;
use crate::timeline::TranscriptEvent;
use anyhow::{Context, Result};
use sherpa_onnx::{
    OfflineQwen3ASRModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    SileroVadModelConfig, VadModelConfig, VoiceActivityDetector, Wave,
};
use std::path::{Path, PathBuf};

pub struct AsrInput {
    pub wav: PathBuf,
    pub vad_model: PathBuf,
    pub qwen: Qwen3Paths,
}

struct Seg {
    start_sample: usize,
    samples: Vec<f32>,
}

/// ASR 阶段入口（阻塞任务）。返回按时间升序的 TranscriptEvent 列表。
pub async fn run(cfg: &PipelineConfig, input: AsrInput) -> Result<Vec<TranscriptEvent>> {
    let threads = cfg.threads;
    let workers = cfg.workers.max(1);
    let vad_threshold = cfg.vad_threshold;
    let max_speech = cfg.max_speech;
    let provider = cfg.provider.clone();
    tokio::task::spawn_blocking(move || {
        run_blocking(&input, threads, vad_threshold, max_speech, workers, provider)
    })
        .await
        .context("ASR 线程 join 失败")?
}

/// 清洗 Qwen3 转写中的提示词残留（如 `**language Chinese<asr_text>` 前缀与 `</asr_text>` 后缀）。
pub fn sanitize_qwen_text(s: &str) -> String {
    let mut t = s.trim();
    // 去掉闭合标记后缀
    if let Some(p) = t.find("</asr_text>") {
        t = t[..p].trim();
    }
    // 去掉最后一个 <asr_text> 及其之前的内容（含 **language 等提示）
    if let Some(p) = t.rfind("<asr_text>") {
        t = t[p + "<asr_text>".len()..].trim();
    }
    t.to_string()
}

fn run_blocking(
    input: &AsrInput,
    threads: i32,
    vad_threshold: f32,
    max_speech: f32,
    workers: usize,
    provider: String,
) -> Result<Vec<TranscriptEvent>> {
    let t0 = std::time::Instant::now();

    let wave = Wave::read(
        input
            .wav
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("wav 路径非 UTF-8"))?,
    )
    .context("读取 audio.wav 失败（sherpa Wave::read）")?;
    let sr = wave.sample_rate() as usize;
    let samples = wave.samples();
    let total = samples.len();
    tracing::info!(secs = format_args!("{:.1}", total as f64 / sr as f64), sr, samples = total, "audio");

    // ---- VAD ----
    let mut vad_cfg = VadModelConfig::default();
    vad_cfg.silero_vad = SileroVadModelConfig {
        model: Some(
            input
                .vad_model
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("VAD 路径非 UTF-8"))?
                .to_string(),
        ),
        threshold: vad_threshold,
        min_silence_duration: 0.5,
        min_speech_duration: 0.25,
        window_size: 512,
        max_speech_duration: max_speech,
    };
    vad_cfg.sample_rate = 16000;
    let vad = VoiceActivityDetector::create(&vad_cfg, 60.0).context("创建 Silero VAD 失败")?;

    let window = vad_cfg.silero_vad.window_size as usize;
    let mut segs: Vec<Seg> = vec![];
    let drain = |vad: &VoiceActivityDetector, segs: &mut Vec<Seg>| loop {
        if vad.is_empty() {
            break;
        }
        if let Some(s) = vad.front() {
            segs.push(Seg {
                start_sample: s.start().max(0) as usize,
                samples: s.samples().to_vec(),
            });
            vad.pop();
        } else {
            break;
        }
    };
    let mut i = 0usize;
    while i + window <= total {
        vad.accept_waveform(&samples[i..i + window]);
        drain(&vad, &mut segs);
        i += window;
    }
    if total > i {
        vad.accept_waveform(&samples[i..]);
    }
    vad.flush();
    drain(&vad, &mut segs);
    let vad_secs = t0.elapsed().as_secs_f64();
    let speech_secs: f64 = segs.iter().map(|s| s.samples.len()).sum::<usize>() as f64 / sr as f64;
    tracing::info!(
        segs = segs.len(),
        speech = format_args!("{:.0}s", speech_secs),
        audio = format_args!("{:.0}s", total as f64 / sr as f64),
        secs = format_args!("{:.1}", vad_secs),
        "vad"
    );

    // Qwen3-ASR（sherpa-onnx）
    // LLM 自回归解码 batch=1，intra-op 并行扩展性差；段间独立，进程内并行收益近线性。
    let q = &input.qwen;
    let make_config = |num_threads: i32| {
        let mut rc = OfflineRecognizerConfig::default();
        rc.model_config.qwen3_asr = OfflineQwen3ASRModelConfig {
            conv_frontend: Some(q.conv_frontend.display().to_string()),
            encoder: Some(q.encoder.display().to_string()),
            decoder: Some(q.decoder.display().to_string()),
            tokenizer: Some(q.tokenizer.display().to_string()),
            max_total_len: 1024,
            max_new_tokens: 1024,
            ..Default::default()
        };
        rc.model_config.tokens = Some(String::new());
        rc.model_config.num_threads = num_threads;
        rc.model_config.provider = Some(provider.clone());
        rc
    };

    let per_worker_threads = (threads / workers as i32).max(1);
    tracing::info!(provider = %provider, workers, threads = per_worker_threads, "onnx asr");
    let t_load = std::time::Instant::now();

    struct SegOut {
        start: f64,
        end: f64,
        text: String,
    }
    let results: std::sync::Mutex<Vec<SegOut>> = Default::default();
    let queue: std::sync::Mutex<std::collections::VecDeque<usize>> =
        std::sync::Mutex::new((0..segs.len()).collect());
    let pb = indicatif::ProgressBar::new(segs.len() as u64);
    pb.set_style(
        indicatif::ProgressStyle::with_template(
            "{spinner:.green} asr {pos}/{len} [{bar:32.cyan/blue}] {elapsed} {msg}",
        )
        .unwrap()
        .progress_chars("##-"),
    );

    let decode_t0 = std::time::Instant::now();
    std::thread::scope(|scope| -> anyhow::Result<()> {
        let mut handles = vec![];
        for w in 0..workers {
            let queue = &queue;
            let results = &results;
            let segs = &segs;
            let pb = pb.clone();
            let rc = make_config(per_worker_threads);
            handles.push(scope.spawn(move || -> anyhow::Result<()> {
                let recognizer = OfflineRecognizer::create(&rc)
                    .context("创建 Qwen3-ASR recognizer 失败（检查模型文件/内存）")?;
                if w == 0 {
                    tracing::info!(secs = format_args!("{:.1}", t_load.elapsed().as_secs_f64()), "recognizer ready");
                }
                loop {
                    let Some(i) = queue.lock().unwrap().pop_front() else {
                        break;
                    };
                    let seg = &segs[i];
                    let stream = recognizer.create_stream();
                    stream.accept_waveform(16000, &seg.samples);
                    recognizer.decode(&stream);
                    let start = seg.start_sample as f64 / sr as f64;
                    let end = start + seg.samples.len() as f64 / sr as f64;
                    if let Some(result) = stream.get_result() {
                        let text = sanitize_qwen_text(&result.text);
                        if !text.is_empty() {
                            results.lock().unwrap().push(SegOut { start, end, text });
                        }
                    }
                    pb.inc(1);
                }
                Ok(())
            }));
        }
        for h in handles {
            h.join().map_err(|_| anyhow::anyhow!("ASR worker 线程 panic"))??;
        }
        Ok(())
    })?;
    pb.finish_and_clear();

    let mut events: Vec<TranscriptEvent> = results
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|o| TranscriptEvent {
            start: o.start,
            end: o.end,
            text: o.text,
        })
        .collect();
    events.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
    let chars: usize = events.iter().map(|e| e.text.chars().count()).sum();
    let el = decode_t0.elapsed().as_secs_f64();
    let rtf = if speech_secs > 0.0 { el / speech_secs } else { 0.0 };
    tracing::info!(
        events = events.len(),
        segs = segs.len(),
        chars,
        workers,
        secs = format_args!("{el:.1}"),
        rtf = format_args!("{rtf:.2}"),
        "asr done"
    );
    Ok(events)
}

/// 供单测/冒烟：对一个 wav 只跑 VAD（不加载大模型），返回段列表 (start_s, dur_s)。
#[allow(dead_code)]
pub fn vad_only(vad_model: &Path, wav: &Path, threshold: f32) -> Result<Vec<(f64, f64)>> {
    let wave = Wave::read(wav.to_str().unwrap()).context("read wav")?;
    let sr = wave.sample_rate() as usize;
    let samples = wave.samples();
    let mut vad_cfg = VadModelConfig::default();
    vad_cfg.silero_vad = SileroVadModelConfig {
        model: Some(vad_model.display().to_string()),
        threshold,
        min_silence_duration: 0.5,
        min_speech_duration: 0.25,
        window_size: 512,
        max_speech_duration: 20.0,
    };
    vad_cfg.sample_rate = 16000;
    let vad = VoiceActivityDetector::create(&vad_cfg, 60.0).context("create vad")?;
    let window = 512usize;
    let mut out = vec![];
    let collect = |vad: &VoiceActivityDetector, out: &mut Vec<(f64, f64)>| loop {
        if vad.is_empty() {
            break;
        }
        if let Some(s) = vad.front() {
            out.push((
                s.start().max(0) as f64 / sr as f64,
                s.samples().len() as f64 / sr as f64,
            ));
            vad.pop();
        } else {
            break;
        }
    };
    let mut i = 0usize;
    while i + window <= samples.len() {
        vad.accept_waveform(&samples[i..i + window]);
        collect(&vad, &mut out);
        i += window;
    }
    if samples.len() > i {
        vad.accept_waveform(&samples[i..]);
    }
    vad.flush();
    collect(&vad, &mut out);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_prompt_artifacts() {
        assert_eq!(
            sanitize_qwen_text("**language Chinese<asr_text>你好世界。"),
            "你好世界。"
        );
        assert_eq!(sanitize_qwen_text("正常文本"), "正常文本");
        assert_eq!(sanitize_qwen_text("内容</asr_text>尾巴"), "内容");
        assert_eq!(sanitize_qwen_text("**language Chinese<asr_text>你好</asr_text>"), "你好");
        assert_eq!(sanitize_qwen_text("  空白  "), "空白");
    }
}
