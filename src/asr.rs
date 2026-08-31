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
    let vad_threshold = cfg.vad_threshold;
    let max_speech = cfg.max_speech;
    tokio::task::spawn_blocking(move || run_blocking(&input, threads, vad_threshold, max_speech))
        .await
        .context("ASR 线程 join 失败")?
}

fn run_blocking(
    input: &AsrInput,
    threads: i32,
    vad_threshold: f32,
    max_speech: f32,
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
    println!(
        "  [asr] 音频 {:.1}s（sr={sr}, samples={total}）",
        total as f64 / sr as f64
    );

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
    let mut drain = |vad: &VoiceActivityDetector, segs: &mut Vec<Seg>| loop {
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
    println!(
        "  [asr] VAD 完成：{} 段，语音共 {:.0}s / {:.0}s（{:.1}s）",
        segs.len(),
        speech_secs,
        total as f64 / sr as f64,
        vad_secs
    );

    // ---- Qwen3-ASR ----
    let q = &input.qwen;
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
    rc.model_config.num_threads = threads;
    let recognizer =
        OfflineRecognizer::create(&rc).context("创建 Qwen3-ASR recognizer 失败（检查模型文件）")?;
    println!(
        "  [asr] recognizer 就绪（{:.1}s），开始解码…",
        t0.elapsed().as_secs_f64()
    );

    let mut events: Vec<TranscriptEvent> = vec![];
    let decode_t0 = std::time::Instant::now();
    for (n, seg) in segs.iter().enumerate() {
        let stream = recognizer.create_stream();
        stream.accept_waveform(16000, &seg.samples);
        recognizer.decode(&stream);
        let start = seg.start_sample as f64 / sr as f64;
        let end = start + seg.samples.len() as f64 / sr as f64;
        if let Some(result) = stream.get_result() {
            let text = result.text.trim().to_string();
            if !text.is_empty() {
                events.push(TranscriptEvent { start, end, text });
            }
        }
        if (n + 1) % 20 == 0 || n + 1 == segs.len() {
            let el = decode_t0.elapsed().as_secs_f64();
            let done_secs: f64 = segs[..=n].iter().map(|s| s.samples.len()).sum::<usize>() as f64
                / sr as f64;
            let rtf = if done_secs > 0.0 { el / done_secs } else { 0.0 };
            println!(
                "  [asr] {}/{} 段，已解码 {:.0}s 音频，RTF={rtf:.2}",
                n + 1,
                segs.len(),
                done_secs
            );
        }
    }
    let chars: usize = events.iter().map(|e| e.text.chars().count()).sum();
    println!(
        "  [asr] 完成：{} 条转写 / {} 段，{} 字符，总耗时 {:.1}s",
        events.len(),
        segs.len(),
        chars,
        t0.elapsed().as_secs_f64()
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
    let mut collect = |vad: &VoiceActivityDetector, out: &mut Vec<(f64, f64)>| loop {
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
