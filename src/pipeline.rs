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
    let llama = models::ensure_llama(&cfg.model_dir)?;
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

    tracing::info!(
        secs = format_args!("{:.1}", t_total.elapsed().as_secs_f64()),
        out = %cfg.out_dir.display(),
        "done"
    );
    Ok(())
}
