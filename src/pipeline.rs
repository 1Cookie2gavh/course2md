//! 编排：下载 → (scene ∥ audio) → asr → merge → render。

use crate::asr::{self, AsrInput};
use crate::config::PipelineConfig;
use crate::fetch::{self, VideoMeta};
use crate::media;
use crate::models::{self, ModelSize};
use crate::render;
use crate::scene;
use crate::timeline;
use anyhow::Result;
use std::time::Instant;

pub async fn run(cfg: &PipelineConfig) -> Result<()> {
    let t_total = Instant::now();
    crate::error::require_cmd("ffmpeg")?;
    crate::error::require_cmd("ffprobe")?;

    tokio::fs::create_dir_all(&cfg.out_dir).await?;
    let local = std::path::Path::new(&cfg.url);
    let is_local = local.is_file();
    if !is_local && !cfg.no_download {
        crate::error::require_cmd("yt-dlp")?;
    }

    let meta_path = cfg.meta_path();
    let dest = cfg.media_path();
    if is_local {
        tracing::info!(path = %local.display(), "local video");
        if dest != local && !dest.is_file() {
            tokio::fs::copy(local, &dest).await?;
        }
    } else if !cfg.no_download {
        tracing::info!("download video (≤720p)");
        fetch::download(&cfg.url, &dest, tracing::enabled!(tracing::Level::DEBUG)).await?;
    } else {
        anyhow::ensure!(dest.is_file(), "--no-download 但 {} 不存在", dest.display());
    }

    let meta: VideoMeta = if meta_path.is_file() {
        tracing::info!("reuse meta.json");
        VideoMeta::load(&meta_path)?
    } else if is_local {
        let dur = media::probe_duration(&dest).await.unwrap_or(0.0);
        let m = VideoMeta {
            title: dest
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("local")
                .to_string(),
            uploader: String::new(),
            duration: dur,
            webpage_url: dest.display().to_string(),
            extractor: "local".into(),
            id: String::new(),
        };
        m.save(&meta_path)?;
        m
    } else {
        tracing::info!("fetch metadata");
        let m = fetch::fetch_meta(&cfg.url).await?;
        m.save(&meta_path)?;
        m
    };
    tracing::info!(
        title = %meta.title,
        uploader = %meta.uploader,
        duration = format_args!("{:.0}s", meta.duration),
        "video"
    );

    tracing::info!("scene detect ∥ audio extract");
    let media = cfg.media_path();
    let audio_path = cfg.audio_path();
    let (frames_res, audio_res) = tokio::join!(
        scene::run(cfg, &media),
        media::extract_audio(&media, &audio_path)
    );
    let frames = frames_res?;
    audio_res?;
    anyhow::ensure!(!frames.is_empty(), "场景检测未保留任何帧");

    tracing::info!(provider = %cfg.provider, precision = %cfg.precision, "asr");
    let (vad_path, qwen_paths) =
        models::ensure_models_prec(&cfg.model_dir, ModelSize::Q17B, &cfg.precision)?;
    let events = asr::run(
        cfg,
        AsrInput {
            wav: cfg.audio_path(),
            vad_model: vad_path,
            qwen: qwen_paths,
        },
    )
    .await?;

    let sections = timeline::merge(frames.clone(), events.clone());
    timeline::write_jsonl(&cfg.timeline_path(), &frames, &events)?;
    tracing::info!(sections = sections.len(), "merged timeline");

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
