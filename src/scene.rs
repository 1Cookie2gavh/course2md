//! 幻灯片抽帧：对齐 yt-slide-mark
//! 每 `sample_interval` 秒取一帧，与上一张保留帧做 SSIM；
//! 低于 `similarity` 则保存，并跳过 `cooldown` 秒。

use crate::config::{PipelineConfig, Roi};
use crate::media;
use crate::timeline::FrameEvent;
use anyhow::{Context, Result};
use image::GrayImage;
use image_compare::Algorithm;
use indicatif::{ProgressBar, ProgressStyle};
use std::path::Path;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

fn scaled_wh(ow: u32, oh: u32, target_w: u32) -> (u32, u32) {
    let h = ((oh as u64 * target_w as u64) / ow.max(1) as u64) as u32;
    (target_w, (h & !1).max(2))
}

fn crop_roi(img: &GrayImage, roi: Option<Roi>) -> GrayImage {
    let Some(r) = roi else {
        return img.clone();
    };
    let (w, h) = img.dimensions();
    let (x1, y1, x2, y2) = r.pixels(w, h);
    image::imageops::crop_imm(img, x1, y1, (x2 - x1).max(1), (y2 - y1).max(1)).to_image()
}

fn ssim(a: &GrayImage, b: &GrayImage) -> f64 {
    image_compare::gray_similarity_structure(&Algorithm::MSSIMSimple, a, b)
        .map(|s| s.score)
        .unwrap_or(0.0)
}

/// 单点精确抽帧（全分辨率 JPEG）。
async fn extract_frame(media: &Path, t: f64, dest: &Path) -> Result<()> {
    let ss = format!("{t:.3}");
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-ss", &ss])
        .arg("-i")
        .arg(media)
        .args(["-frames:v", "1", "-q:v", "2"])
        .arg(dest)
        .output()
        .await
        .context("启动 ffmpeg 抽帧失败")?;
    if !out.status.success() {
        return Err(crate::error::cmd_error(
            "ffmpeg",
            out.status.code(),
            &String::from_utf8_lossy(&out.stderr),
        ));
    }
    Ok(())
}

/// 1fps（或 sample_interval）灰度流 + SSIM，得到新幻灯片时间点。
async fn sample_timestamps(cfg: &PipelineConfig, media: &Path) -> Result<Vec<f64>> {
    let info = media::probe_video(media)
        .await
        .context("ffprobe 无法读取视频宽高")?;
    let interval = cfg.sample_interval.max(0.2);
    let (tw, th) = scaled_wh(info.width, info.height, 640);
    let fps = 1.0 / interval;
    let vf = format!("fps={fps:.6},scale={tw}:{th},format=gray");
    let total = ((info.duration / interval).ceil() as u64).max(1);

    tracing::info!(
        w = info.width,
        h = info.height,
        duration = format_args!("{:.0}s", info.duration),
        interval,
        similarity = cfg.similarity,
        cooldown = cfg.cooldown,
        "slide sample"
    );

    let mut child = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-an"])
        .arg("-i")
        .arg(media)
        .args(["-vf", &vf, "-f", "rawvideo", "pipe:1"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("启动 ffmpeg 采样失败")?;
    let mut stdout = child.stdout.take().context("ffmpeg stdout")?;
    let frame_len = (tw as usize) * (th as usize);
    let mut buf = vec![0u8; frame_len];
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template("{spinner:.green} sample {pos}/{len} [{bar:32.cyan/blue}] {msg}")
            .unwrap()
            .progress_chars("##-"),
    );

    // 三状态检测：检测永不休眠（cooldown 只限制「发射」，不再造成盲区）。
    //   last_emitted  已输出的视觉状态
    //   candidate     正在观察的候选画面（含首次出现时间，用于真实时间戳）
    // 发射条件：候选与上一输出差异显著 + 稳定时长足够 + 距上次发射 >= cooldown。
    let stable_for = if cfg.slide_mode == "stable" { cfg.stable_secs } else { 0.0 };
    let mut times = vec![];
    let mut last_emitted: Option<GrayImage> = None;
    let mut candidate: Option<GrayImage> = None;
    let mut candidate_first_t: Option<f64> = None;
    let mut last_emit_t: f64 = -f64::INFINITY;
    let mut i: u64 = 0;
    loop {
        match stdout.read_exact(&mut buf).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        let t = i as f64 * interval;
        i += 1;
        pb.inc(1);
        let gray = GrayImage::from_raw(tw, th, buf.clone())
            .ok_or_else(|| anyhow::anyhow!("灰度帧尺寸不匹配 {tw}x{th}"))?;
        let cmp = crop_roi(&gray, cfg.roi);

        let differs_from_emitted = match &last_emitted {
            None => true,
            Some(prev) => {
                prev.dimensions() != cmp.dimensions() || ssim(prev, &cmp) < cfg.similarity
            }
        };
        if !differs_from_emitted {
            // 画面回到已输出状态：候选是过渡帧（动画/抖动），丢弃
            candidate = None;
            candidate_first_t = None;
            continue;
        }
        // 与已输出状态不同：跟踪候选（若候选本身又变了，说明动画进行中，重置起点）
        let candidate_changed = match &candidate {
            None => true,
            Some(c) => c.dimensions() != cmp.dimensions() || ssim(c, &cmp) < cfg.similarity,
        };
        if candidate_changed {
            candidate = Some(cmp);
            candidate_first_t = Some(t);
        }
        let first_t = candidate_first_t.unwrap_or(t);
        let stable = t - first_t >= stable_for;
        let gap_ok = t - last_emit_t >= cfg.cooldown;
        if stable && gap_ok {
            // 用候选首次出现的时间戳，而不是 cooldown 到期时间
            times.push(first_t);
            last_emitted = candidate.take();
            candidate_first_t = None;
            last_emit_t = t;
            pb.set_message(format!("slides={} t={first_t:.1}s", times.len()));
        }
    }
    pb.finish_and_clear();
    let _ = child.wait().await;
    tracing::info!(slides = times.len(), frames = i, mode = %cfg.slide_mode, "ssim scan done");
    Ok(times)
}

/// 场景阶段入口。
pub async fn run(cfg: &PipelineConfig, media: &Path) -> Result<Vec<FrameEvent>> {
    let frames_dir = cfg.frames_dir();
    tokio::fs::create_dir_all(&frames_dir).await?;
    let t0 = std::time::Instant::now();
    let times = sample_timestamps(cfg, media).await?;
    anyhow::ensure!(!times.is_empty(), "未采样到任何帧");

    let pb = ProgressBar::new(times.len() as u64);
    pb.set_style(
        ProgressStyle::with_template("{spinner:.green} extract {pos}/{len} [{bar:32.cyan/blue}] {msg}")
            .unwrap()
            .progress_chars("##-"),
    );
    let mut frames = vec![];
    for (i, &t) in times.iter().enumerate() {
        let name = format!("slide_{:04}.jpg", i + 1);
        let path = frames_dir.join(&name);
        extract_frame(media, t, &path).await?;
        frames.push(FrameEvent {
            t,
            image: format!("frames/{name}"),
        });
        pb.set_message(format!("t={t:.1}s"));
        pb.inc(1);
    }
    pb.finish_and_clear();
    tracing::info!(
        frames = frames.len(),
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "slides extracted"
    );
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_keeps_even_height() {
        let (w, h) = scaled_wh(1280, 410, 640);
        assert_eq!(w, 640);
        assert_eq!(h % 2, 0);
    }
}
