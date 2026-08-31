//! run 子命令的编排：下载 → (scene ∥ audio) → asr → merge → render。

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
    crate::error::require_cmd("yt-dlp")?;

    tokio::fs::create_dir_all(&cfg.out_dir).await?;

    // ---- 1. 元数据 / 下载 ----
    let meta_path = cfg.meta_path();
    let meta: VideoMeta = if meta_path.is_file() {
        println!("  [fetch] 复用已有 meta.json");
        VideoMeta::load(&meta_path)?
    } else {
        println!("  [fetch] 抓取元数据…");
        let m = fetch::fetch_meta(&cfg.url).await?;
        m.save(&meta_path)?;
        println!("  [fetch] 《{}》 时长 {:.0}s", m.title, m.duration);
        m
    };

    if !cfg.no_download {
        println!("  [fetch] 下载视频（≤720p）…");
        fetch::download(&cfg.url, &cfg.media_path(), false).await?;
    } else {
        anyhow::ensure!(
            cfg.media_path().is_file(),
            "--no-download 但 {} 不存在",
            cfg.media_path().display()
        );
    }

    // ---- 2. scene 与 audio 并行 ----
    println!("  [stage] 场景检测与音频抽取并行开始");
    let media = cfg.media_path();
    let audio_path = cfg.audio_path();
    let (frames_res, audio_res) = tokio::join!(
        scene::run(cfg, &media),
        media::extract_audio(&media, &audio_path)
    );
    let frames = frames_res?;
    audio_res?;
    anyhow::ensure!(!frames.is_empty(), "场景检测未保留任何帧");

    // ---- 3. ASR ----
    let (vad_path, qwen_paths) =
        models::ensure_models(&cfg.model_dir, ModelSize::Q17B)?;
    let events = asr::run(
        cfg,
        AsrInput {
            wav: cfg.audio_path(),
            vad_model: vad_path,
            qwen: qwen_paths,
        },
    )
    .await?;

    // ---- 4. 合并 + timeline.jsonl ----
    let sections = timeline::merge(frames.clone(), events.clone());
    timeline::write_jsonl(&cfg.timeline_path(), &frames, &events)?;
    println!(
        "  [merge] {} 节（timeline.jsonl 已写入）",
        sections.len()
    );

    // ---- 5. 渲染 ----
    render::write_outputs(&cfg.out_dir, &meta, &sections, &cfg.formats).await?;
    if !cfg.keep_video {
        let _ = tokio::fs::remove_file(&media).await;
    }

    println!(
        "\n完成 ✅  耗时 {:.1}s → {}",
        t_total.elapsed().as_secs_f64(),
        cfg.out_dir.display()
    );
    Ok(())
}
