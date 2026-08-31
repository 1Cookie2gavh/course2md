//! 编排：元数据 → 目录 → 下载 → (截图 ∥ 音频) → 识别 → 渲染。

use crate::asr::{self, AsrInput};
use crate::config::{self, PipelineConfig};
use crate::fetch::{self, VideoMeta};
use crate::media;
use crate::models;
use crate::render;
use crate::scene;
use crate::timeline;
use anyhow::{Context, Result};
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
    // 本地文件直接原地处理，不拷贝；下载类输入落到 dest。
    let media: std::path::PathBuf = if is_local {
        tracing::info!(path = %local.display(), "local video");
        local.to_path_buf()
    } else if !cfg.no_download {
        tracing::info!("download video");
        fetch::download(&cfg.url, &dest, tracing::enabled!(tracing::Level::DEBUG)).await?;
        dest
    } else {
        anyhow::ensure!(dest.is_file(), "--no-download 但 {} 不存在", dest.display());
        dest
    };

    tracing::info!("extract slides and audio");
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

    // 可选的 LLM 润色：尽早校验配置，避免跑完识别才发现配错
    let events = if cfg.llm.enabled {
        crate::llm::validate(&cfg.llm)?;
        tracing::info!(model = %cfg.llm.model, "llm polish");
        let ev = cfg.llm.clone();
        let joined = tokio::task::spawn_blocking(move || crate::llm::polish(events, &ev)).await;
        joined.context("LLM 线程 join 失败")?
    } else {
        events
    };

    let sections = timeline::merge(frames.clone(), events.clone());
    timeline::write_jsonl(&cfg.timeline_path(), &frames, &events)?;
    tracing::info!(sections = sections.len(), "merged");

    render::write_outputs(&cfg.out_dir, &meta, &sections, &cfg.formats).await?;
    // 只删自己下载的视频；本地输入文件不动。
    if !cfg.keep_video && media != local {
        let _ = tokio::fs::remove_file(&media).await;
    }

    #[cfg(unix)]
    let (peak_mb, child_peak_mb) = (
        peak_rss_mb(libc::RUSAGE_SELF),
        peak_rss_mb(libc::RUSAGE_CHILDREN),
    );
    #[cfg(not(unix))]
    let (peak_mb, child_peak_mb) = (None, None);
    let stats = RunStats {
        elapsed_secs: t_total.elapsed().as_secs_f64(),
        peak_mb,
        child_peak_mb,
    };
    print_summary(&cfg, &meta, &sections, &stats, &media, is_local);
    Ok(())
}

struct RunStats {
    elapsed_secs: f64,
    peak_mb: Option<f64>,
    child_peak_mb: Option<f64>,
}

fn print_summary(
    cfg: &PipelineConfig,
    meta: &VideoMeta,
    sections: &[timeline::Section],
    stats: &RunStats,
    media: &Path,
    is_local: bool,
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
    if is_local {
        eprintln!("视频：{}  （本地输入，未改动）", media.display());
    } else if cfg.keep_video {
        eprintln!("视频：{}  （已保留）", media.display());
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
    eprintln!("耗时：{}", fmt_duration(stats.elapsed_secs));
    match (stats.peak_mb, stats.child_peak_mb) {
        (Some(mb), Some(c)) => eprintln!(
            "峰值内存：{mb:.0} MB（course2md）+ 最大子进程 {c:.0} MB（llama-server/ffmpeg 等）"
        ),
        (Some(mb), None) => eprintln!("峰值内存（本进程 RSS）：{mb:.0} MB"),
        _ => eprintln!("峰值内存：不可用"),
    }
    eprintln!("模型目录：{}", cfg.model_dir.display());
    eprintln!("──────────────────────────────");
    if !cfg.llm.enabled && !cfg.llm.disable_hint {
        crate::llm::write_hint_note(&crate::llm::config_path());
    }
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

/// 峰值常驻集（Linux 为 KB，macOS 为字节）。RUSAGE_CHILDREN 口径含 llama-server/ffmpeg。
fn peak_rss_mb(who: libc::c_int) -> Option<f64> {
    #[cfg(unix)]
    {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        let rc = unsafe { libc::getrusage(who, usage.as_mut_ptr()) };
        if rc != 0 {
            return None;
        }
        let usage = unsafe { usage.assume_init() };
        let rss = usage.ru_maxrss as f64;
        #[cfg(target_os = "macos")]
        {
            Some(rss / (1024.0 * 1024.0))
        }
        #[cfg(not(target_os = "macos"))]
        {
            Some(rss / 1024.0)
        }
    }
    #[cfg(not(unix))]
    {
        None
    }
}
