//! 编排：元数据 → 目录 → 下载 → (截图 ∥ 音频) → 识别 → 渲染。

use crate::asr::{self, AsrInput};
use crate::config::{self, PipelineConfig};
use crate::fetch::{self, VideoMeta};
use crate::media;
use crate::models;
use crate::render;
use crate::scene;
use crate::timeline;
use anyhow::Result;
use std::path::Path;
use std::time::Instant;

pub async fn run(cfg: &PipelineConfig) -> Result<()> {
    let t_total = Instant::now();
    crate::error::require_cmd("ffmpeg")?;
    crate::error::require_cmd("ffprobe")?;
    crate::error::require_cmd("llama-server")?;

    let local = Path::new(&cfg.url);
    let is_local = local.is_file();
    if !is_local && !cfg.no_download {
        crate::error::require_cmd("yt-dlp")?;
    }

    let mut cfg = cfg.clone();

    let meta = if is_local {
        let dur = media::probe_duration(local).await.unwrap_or(0.0);
        let stem = local
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("local")
            .to_string();
        VideoMeta {
            title: stem.clone(),
            uploader: String::new(),
            duration: dur,
            webpage_url: local.display().to_string(),
            extractor: "local".into(),
            id: stem,
        }
    } else {
        tracing::info!("fetch metadata");
        fetch::fetch_meta(&cfg.url).await?
    };

    let id = if meta.id.is_empty() {
        config::infer_slug(&cfg.url)
    } else {
        meta.id.clone()
    };
    let title = if meta.title.is_empty() {
        id.clone()
    } else {
        meta.title.clone()
    };
    let platform = config::platform_from(&cfg.url, &meta.extractor);
    cfg.out_dir = config::course_dir(&cfg.out_root, &platform, &title, &id);
    tokio::fs::create_dir_all(&cfg.out_dir).await?;
    meta.save(&cfg.meta_path())?;
    tracing::info!(
        title = %meta.title,
        platform = %platform,
        id = %id,
        out = %cfg.out_dir.display(),
        duration = format_args!("{:.0}s", meta.duration),
        "video"
    );

    let dest = cfg.media_path();
    if is_local {
        tracing::info!(path = %local.display(), "local video");
        if dest != local && !dest.is_file() {
            tokio::fs::copy(local, &dest).await?;
        }
    } else if !cfg.no_download {
        tracing::info!("download video");
        fetch::download(&cfg.url, &dest, tracing::enabled!(tracing::Level::DEBUG)).await?;
    } else {
        anyhow::ensure!(dest.is_file(), "--no-download 但 {} 不存在", dest.display());
    }

    tracing::info!("extract slides and audio");
    let media = cfg.media_path();
    let audio_path = cfg.audio_path();
    let (frames_res, audio_res) = tokio::join!(
        scene::run(&cfg, &media),
        media::extract_audio(&media, &audio_path)
    );
    let frames = frames_res?;
    audio_res?;
    anyhow::ensure!(!frames.is_empty(), "没有截到任何画面");

    tracing::info!(device = %cfg.provider, "transcribe");
    let llama = models::ensure_llama_or_download(&cfg.model_dir).await?;
    let events = asr::run(
        &cfg,
        AsrInput {
            wav: cfg.audio_path(),
            model: llama.model,
            mmproj: llama.mmproj,
        },
    )
    .await?;

    let sections = timeline::merge(frames.clone(), events.clone());
    timeline::write_jsonl(&cfg.timeline_path(), &frames, &events)?;
    tracing::info!(sections = sections.len(), "merged");

    render::write_outputs(&cfg.out_dir, &meta, &sections, &cfg.formats).await?;
    if !cfg.keep_video {
        let _ = tokio::fs::remove_file(&media).await;
    }

    let elapsed = t_total.elapsed().as_secs_f64();
    let peak = peak_rss_mb();
    print_summary(&cfg, &meta, &sections, elapsed, peak);
    Ok(())
}

fn print_summary(
    cfg: &PipelineConfig,
    meta: &VideoMeta,
    sections: &[timeline::Section],
    elapsed_secs: f64,
    peak_mb: Option<f64>,
) {
    let out = &cfg.out_dir;
    let speech_n: usize = sections.iter().map(|s| s.speech.len()).sum();
    let chars: usize = sections
        .iter()
        .flat_map(|s| s.speech.iter())
        .map(|e| e.text.chars().count())
        .sum();

    eprintln!();
    eprintln!("──────── course2md 完成 ────────");
    eprintln!("标题：{}", meta.title);
    eprintln!("输出目录：{}", out.display());
    eprintln!();
    eprintln!("文稿：");
    for f in &cfg.formats {
        let name = match f.as_str() {
            "md" => "course.md",
            "html" => "course.html",
            "json" => "structured.json",
            other => other,
        };
        let p = out.join(name);
        if p.is_file() {
            eprintln!("  {}", p.display());
        }
    }
    eprintln!("截图：{}/frames/  （{} 张）", out.display(), sections.len());
    eprintln!("音频：{}", cfg.audio_path().display());
    if cfg.keep_video {
        eprintln!("视频：{}  （已保留）", cfg.media_path().display());
    } else {
        eprintln!("视频：已删除（需要时加 --keep-video）");
    }
    eprintln!("时间线：{}", cfg.timeline_path().display());
    eprintln!();
    eprintln!(
        "统计：{} 张截图 / {} 段语音 / {} 字",
        sections.len(),
        speech_n,
        chars
    );
    eprintln!("耗时：{}", fmt_duration(elapsed_secs));
    match peak_mb {
        Some(mb) => eprintln!("峰值内存（本进程 RSS）：{mb:.0} MB"),
        None => eprintln!("峰值内存：不可用"),
    }
    eprintln!("模型目录：{}", cfg.model_dir.display());
    eprintln!("──────────────────────────────");
}

fn fmt_duration(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m{:02}s", s / 3600, (s % 3600) / 60, s % 60)
    }
}

/// 本进程峰值常驻集。Linux 为 KB，macOS 为字节。不含 llama-server 子进程显存。
fn peak_rss_mb() -> Option<f64> {
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
        if rc != 0 {
            return None;
        }
        let usage = unsafe { usage.assume_init() };
        let rss = usage.ru_maxrss as f64;
        #[cfg(target_os = "macos")]
        {
            return Some(rss / (1024.0 * 1024.0));
        }
        #[cfg(not(target_os = "macos"))]
        {
            return Some(rss / 1024.0);
        }
    }
    #[cfg(not(unix))]
    {
        None
    }
}
