//! Apple 原生后端（仅 `apple_native` 构建生效）：
//! Silero VAD（CoreML/ANE）+ Qwen3-ASR（CoreML，来自 speech-swift）。
//! 模型首次使用时自动下载到 ~/Library/Caches/qwen3-speech/（HF_ENDPOINT 可换镜像）。

#![cfg(apple_native)]

use crate::timeline::TranscriptEvent;
use anyhow::{Context, Result};
use std::ffi::{CStr, CString};
use std::path::Path;
use std::time::Instant;

mod ffi {
    use std::os::raw::{c_char, c_double, c_int};

    unsafe extern "C" {
        pub fn c2m_vad_detect(
            wav_path: *const c_char,
            min_speech: c_double,
            min_silence: c_double,
            out_starts: *mut *mut c_double,
            out_ends: *mut *mut c_double,
            out_n: *mut c_int,
        ) -> c_int;
        pub fn c2m_free_doubles(p: *mut c_double);
        pub fn c2m_asr_create(model: *const c_char, err: *mut c_char, err_len: usize) -> *mut std::ffi::c_void;
        pub fn c2m_asr_transcribe(
            handle: *mut std::ffi::c_void,
            wav_path: *const c_char,
            out_text: *mut c_char,
            out_len: usize,
        ) -> c_int;
        pub fn c2m_asr_destroy(handle: *mut std::ffi::c_void);
        pub fn c2m_last_error() -> *const c_char;
    }
}

fn last_error() -> String {
    unsafe {
        let p = ffi::c2m_last_error();
        if p.is_null() {
            String::new()
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    }
}

/// Silero VAD（CoreML）。返回 (start, end) 语音段（秒）。
pub fn vad(wav: &Path, min_speech: f64, min_silence: f64) -> Result<Vec<(f64, f64)>> {
    let path = CString::new(wav.to_string_lossy().as_bytes())?;
    let mut starts: *mut f64 = std::ptr::null_mut();
    let mut ends: *mut f64 = std::ptr::null_mut();
    let mut n: i32 = 0;
    let rc = unsafe {
        ffi::c2m_vad_detect(
            path.as_ptr(),
            min_speech,
            min_silence,
            &mut starts,
            &mut ends,
            &mut n,
        )
    };
    if rc != 0 {
        anyhow::bail!("Silero VAD 失败: {}", last_error());
    }
    let mut out = Vec::with_capacity(n as usize);
    if n > 0 {
        unsafe {
            for i in 0..n as usize {
                out.push((starts.add(i).read(), ends.add(i).read()));
            }
            ffi::c2m_free_doubles(starts);
            ffi::c2m_free_doubles(ends);
        }
    }
    Ok(out)
}

pub struct CoremlAsr {
    handle: *mut std::ffi::c_void,
}

/// MLX 要求 metallib 与可执行文件同目录；缺失时给出明确指引而不是 C++ 崩溃。
fn ensure_metallib() -> Result<()> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().unwrap_or(std::path::Path::new("."));
    for name in ["mlx.metallib", "default.metallib"] {
        if dir.join(name).is_file() {
            return Ok(());
        }
    }
    anyhow::bail!(
        "缺少 MLX Metal 库（{}），CoreML 推理不可用。\n\
         从源码构建：把 native/apple-asr/.build/out/Products/Release/mlx-swift_Cmlx.bundle/Contents/Resources/default.metallib \
         复制为二进制同目录的 mlx.metallib；预编译安装：重跑 install.sh。",
        dir.join("mlx.metallib").display()
    )
}

/// 解析 coreml 后端用的模型：显式指定 > 标记文件 > （交互式终端则询问并记忆）> qwen3。
pub fn resolve_model(explicit: Option<&str>) -> String {
    if let Some(m) = explicit {
        return normalize(m);
    }
    let marker = crate::config::config_dir().join("asr_model");
    if let Ok(s) = std::fs::read_to_string(&marker) {
        let m = normalize(s.trim());
        if m == "qwen3" || m == "whisper" {
            return m;
        }
    }
    let chosen = prompt_model_choice();
    let _ = std::fs::write(&marker, &chosen);
    chosen
}

fn normalize(s: &str) -> String {
    let s = s.trim().to_ascii_lowercase();
    if s.contains("whisper") {
        "whisper".into()
    } else {
        "qwen3".into()
    }
}

/// 首次使用：让用户选择下载哪个模型（非交互环境默认 qwen3）。
fn prompt_model_choice() -> String {
    if !atty_or_tty() {
        tracing::info!("非交互环境，默认使用 qwen3 模型（--asr-model whisper 可切换）");
        return "qwen3".into();
    }
    use std::io::{BufRead, Write};
    let mut out = std::io::stderr();
    let _ = writeln!(
        out,
        "选择 CoreML 识别模型 / Select CoreML ASR model:"
    );
    let _ = writeln!(out, "  1) qwen3    - Qwen3-ASR 0.6B（默认 / default，约 1-2GB）");
    let _ = writeln!(out, "  2) whisper  - Whisper large-v3-turbo（多语言 / multilingual）");
    let _ = write!(out, "输入序号并回车（默认 1）/ Enter choice [1]: ");
    let _ = out.flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).unwrap_or(0) == 0 {
        return "qwen3".into();
    }
    match line.trim() {
        "2" => "whisper".into(),
        _ => "qwen3".into(),
    }
}

fn atty_or_tty() -> bool {
    // stderr 接终端才算交互（stdin 可能被重定向）
    unsafe { libc::isatty(2) == 1 }
}

impl CoremlAsr {
    /// 加载模型（首次会自动下载，约 1-2GB）。
    pub fn load(model: &str) -> Result<Self> {
        let name = CString::new(model)?.into_raw();
        let mut err = vec![0u8; 1024];
        let handle = unsafe { ffi::c2m_asr_create(name, err.as_mut_ptr() as *mut _, err.len()) };
        unsafe { std::mem::drop(CString::from_raw(name)) };
        if handle.is_null() {
            let msg = CStr::from_bytes_until_nul(&err)
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            anyhow::bail!("{msg}");
        }
        Ok(Self { handle })
    }

    /// 转写 16k 单声道 wav。Ok(None) = 无语音内容。
    pub fn transcribe(&self, wav: &Path) -> Result<Option<String>> {
        let path = CString::new(wav.to_string_lossy().as_bytes())?;
        let mut out = vec![0u8; 16 * 1024];
        let rc = unsafe {
            ffi::c2m_asr_transcribe(self.handle, path.as_ptr(), out.as_mut_ptr() as *mut _, out.len())
        };
        match rc {
            0 => {
                let s = CStr::from_bytes_until_nul(&out)
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Ok(Some(s))
            }
            1 => Ok(None),
            _ => anyhow::bail!("CoreML 转写失败: {}", last_error()),
        }
    }
}

impl Drop for CoremlAsr {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { ffi::c2m_asr_destroy(self.handle) };
        }
    }
}

// SAFETY: 句柄仅在此线程使用（ASR 全程在同一个阻塞线程内）。
unsafe impl Send for CoremlAsr {}

/// CoreML 全流程：Silero VAD 分段 → 逐段转写。
pub fn run_coreml(
    wav: &Path,
    max_speech: f64,
    model: &str,
    cut: impl Fn(&Path, f64, f64, &Path) -> Result<()>,
    tmp_dir: &Path,
    cp: &mut crate::checkpoint::Checkpoint,
) -> Result<Vec<TranscriptEvent>> {
    let t0 = Instant::now();
    let raw = vad(wav, 0.25, 0.35)?;
    let segs = crate::asr::normalize_segments(raw, max_speech, wav)?;
    tracing::info!(segs = segs.len(), engine = "silero-coreml", "vad");
    if segs.is_empty() {
        tracing::warn!("未检测到语音（VAD 结果为空），跳过识别");
        return Ok(vec![]);
    }

    ensure_metallib()?;
    tracing::info!(model, "loading CoreML ASR（首次使用会自动下载模型）");
    let asr = CoremlAsr::load(model).context("CoreML 模型加载失败")?;
    tracing::info!(secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()), "coreml ready");

    let pb = indicatif::ProgressBar::new(segs.len() as u64);
    pb.set_style(
        indicatif::ProgressStyle::with_template(
            "{spinner:.green} asr {pos}/{len} [{bar:32.cyan/blue}] {elapsed} {msg}",
        )
        .unwrap()
        .progress_chars("##-"),
    );

    let mut events = vec![];
    for (i, seg) in segs.iter().copied().enumerate() {
        let (start, end) = (seg.start, seg.end);
        if cp.is_done(start, end) {
            pb.inc(1);
            continue; // 断点续跑
        }
        let chunk = tmp_dir.join(format!("c{i:04}.wav"));
        cut(wav, seg.cut_start, seg.cut_end, &chunk)?;
        match asr.transcribe(&chunk) {
            Ok(Some(text)) => {
                let text = crate::asr::sanitize_qwen_text(&text);
                if !text.is_empty() {
                    cp.record(start, end, &text);
                    events.push(TranscriptEvent { start, end, text, raw: None });
                }
            }
            Ok(None) => {}
            Err(e) => {
                let _ = std::fs::remove_file(&chunk);
                pb.finish_and_clear();
                return Err(e);
            }
        }
        let _ = std::fs::remove_file(&chunk);
        pb.inc(1);
    }
    pb.finish_and_clear();
    // resume：合并历史 + 本次
    let mut all: Vec<TranscriptEvent> = cp.events().to_vec();
    all.extend(events);
    all.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
    tracing::info!(n = all.len(), secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()), "asr done");
    Ok(all)
}
