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
async fn sample_timestamps(cfg: &PipelineConfig, media: &Path) -> Result<Vec<(f64, f64)>> {
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
    // stderr 必须全程 drain：piped 而不读的话，写满 64KB 管道缓冲会死锁。
    // 缓存尾部供失败诊断，debug 时逐行转发。
    let mut stderr_buf: Vec<u8> = Vec::new();
    let mut stderr = child.stderr.take().context("ffmpeg stderr")?;
    let stderr_task = tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let _ = stderr.read_to_end(&mut stderr_buf).await;
        stderr_buf
    });
    let frame_len = (tw as usize) * (th as usize);
    let mut buf = vec![0u8; frame_len];
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} sample {pos}/{len} [{bar:32.cyan/blue}] {msg}",
        )
        .unwrap()
        .progress_chars("##-"),
    );

    // 三状态检测：检测永不休眠（cooldown 只限制「发射」，不再造成盲区）。
    //   last_emitted  已输出的视觉状态
    //   candidate     正在观察的候选画面（含首次出现时间，用于真实时间戳）
    // 发射条件：候选与上一输出差异显著 + 稳定时长足够 + 距上次发射 >= cooldown。
    let stable_for = if matches!(cfg.slide_mode, crate::config::SlideMode::Stable) {
        cfg.stable_secs
    } else {
        0.0
    };
    let mut times: Vec<(f64, f64)> = vec![];
    let mut last_emitted: Option<GrayImage> = None;
    let mut candidate: Option<GrayImage> = None;
    let mut candidate_first_t: Option<f64> = None;
    let mut candidate_last_t: Option<f64> = None;
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
            candidate_last_t = Some(t);
        } else if candidate_last_t.is_some() {
            // 同一视觉状态的后续采样：更新代表帧时间（取稳定后的最后一帧）
            candidate_last_t = Some(t);
        }
        let onset_t = candidate_first_t.unwrap_or(t);
        let capture_t = candidate_last_t.unwrap_or(t);
        let stable = t - onset_t >= stable_for;
        let gap_ok = t - last_emit_t >= cfg.cooldown;
        if stable && gap_ok {
            // 发射两个时间戳：onset 用于时间线对齐，capture 用于全分辨率截帧
            // （stable 模式下 capture 取确认稳定时的代表帧，避开 transition 早期态）
            times.push((onset_t, capture_t));
            last_emitted = candidate.take();
            candidate_first_t = None;
            candidate_last_t = None;
            last_emit_t = t;
            pb.set_message(format!("slides={} t={onset_t:.1}s", times.len()));
        }
    }
    // EOF 冲刷：最后一个已稳定的候选即使还没过 cooldown 也补发（否则尾页永远丢失）
    if let (Some(_), Some(onset), Some(capture)) = (&candidate, candidate_first_t, candidate_last_t)
    {
        let stable = capture - onset >= stable_for;
        if stable {
            times.push((onset, capture));
        }
    }
    pb.finish_and_clear();
    let status = child.wait().await?;
    let stderr_bytes = stderr_task.await.unwrap_or_default();
    if !status.success() {
        let tail = String::from_utf8_lossy(&stderr_bytes)
            .lines()
            .rev()
            .take(5)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        anyhow::bail!("ffmpeg 采样进程异常退出（{status}）：{tail}");
    }
    tracing::info!(slides = times.len(), frames = i, mode = %cfg.slide_mode, "ssim scan done");
    Ok(times)
}

/// 场景阶段入口。
pub async fn run(cfg: &PipelineConfig, media: &Path) -> Result<Vec<FrameEvent>> {
    let frames_dir = cfg.frames_dir();
    tokio::fs::create_dir_all(&frames_dir).await?;
    let t0 = std::time::Instant::now();
    let mut times = sample_timestamps(cfg, media).await?;
    if times.is_empty() {
        // 画面变化少的视频（访谈/对话/单一场景/快剪）：SSIM 检不出稳定的幻灯片。
        // 兜底按固定间隔抽帧，保证截图与分段字幕照常产出，不再整条任务失败。
        let info = media::probe_video(media)
            .await
            .context("ffprobe 无法读取视频信息")?;
        let dur = info.duration.max(1.0);
        let cooldown = cfg.cooldown.max(1.0);
        // 帧数上限 120，间隔不小于 cooldown
        let step = (dur / 120.0).ceil().max(cooldown);
        let mut t = 0.0;
        while t < dur - 0.25 {
            times.push((t, t));
            t += step;
        }
        if times.is_empty() {
            times.push((0.0, 0.0));
        }
        tracing::warn!(
            count = times.len(),
            step = format_args!("{step:.1}s"),
            "SSIM 未检出幻灯片，改用固定间隔抽帧兜底"
        );
    }

    let pb = ProgressBar::new(times.len() as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} extract {pos}/{len} [{bar:32.cyan/blue}] {msg}",
        )
        .unwrap()
        .progress_chars("##-"),
    );
    let mut frames = vec![];
    for (i, &(onset_t, capture_t)) in times.iter().enumerate() {
        let name = format!("slide_{:04}.jpg", i + 1);
        let path = frames_dir.join(&name);
        // 截帧用代表帧时间（稳定后），时间线用 onset（首次出现）
        extract_frame(media, capture_t, &path).await?;
        frames.push(FrameEvent {
            t: onset_t,
            image: format!("frames/{name}"),
        });
        pb.set_message(format!("t={onset_t:.1}s"));
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
