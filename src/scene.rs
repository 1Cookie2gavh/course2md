//! 场景检测与抽帧：
//! 1) ffmpeg `select=gt(scene,τ)` 过一遍（低分辨率解码）拿候选时间点与分数；
//! 2) 冷却期去抖；
//! 3) 逐点 `-ss` 精确抽帧；
//! 4) 可选 ROI + dHash 去重。

use crate::config::PipelineConfig;
use crate::img_hash::{dhash_file, hamming};
use crate::timeline::FrameEvent;
use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use std::path::Path;
use tokio::process::Command;

/// select+metadata 输出解析出的原始候选。
#[derive(Debug, Clone, Copy)]
struct Candidate {
    t: f64,
    score: f64,
}

/// 解析 `metadata=print` 输出：`pts_time:X` 与 `lavfi.scene_score=Y` 交替出现。
fn parse_candidates(stdout: &str) -> Vec<Candidate> {
    let mut out = vec![];
    let mut pending_t: Option<f64> = None;
    for line in stdout.lines() {
        let line = line.trim();
        // pts_time 出现在行中（如 "frame:10 pts:4000 pts_time:4.000"）
        let pts_t = line.split("pts_time:").nth(1).map(|v| v.trim().parse::<f64>().ok()).flatten();
        if pts_t.is_some() {
            pending_t = pts_t;
        } else if let Some(v) = line
            .strip_prefix("lavfi.scene_score=")
            .or_else(|| line.strip_prefix("lavfi.scd.score="))
        {
            if let (Some(t), Ok(s)) = (pending_t, v.trim().parse::<f64>()) {
                out.push(Candidate { t, score: s });
            }
            pending_t = None;
        }
    }
    out
}

/// Pass 1：场景检测（缩小解码提速）。
async fn detect_scenes(media: &Path, threshold: f64) -> Result<Vec<Candidate>> {
    // fps=1：只抽 1 帧/秒（课件翻页粒度足够）；videotoolbox 走 GPU 硬解。
    let vf = format!(
        "fps=1,scale=640:-2,select='gt(scene,{threshold})',metadata=print:file=-"
    );
    let out = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostats",
            "-hwaccel",
            "videotoolbox",
            "-an",
        ])
        .arg("-i")
        .arg(media)
        .args(["-vf", &vf, "-f", "null", "-"])
        .output()
        .await
        .context("启动 ffmpeg 场景检测失败")?;
    if !out.status.success() {
        return Err(crate::error::cmd_error(
            "ffmpeg",
            out.status.code(),
            &String::from_utf8_lossy(&out.stderr),
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(parse_candidates(&stdout))
}

/// 冷却去抖：首帧 t=0 恒保留；与上一保留点间隔 < cooldown 的候选丢弃。
fn apply_cooldown(cands: &[Candidate], cooldown: f64) -> Vec<f64> {
    let mut kept: Vec<f64> = vec![0.0];
    for c in cands {
        if c.t <= 0.0 {
            continue; // 首帧已保留
        }
        if c.t - kept.last().copied().unwrap_or(0.0) >= cooldown {
            kept.push(c.t);
        }
    }
    kept
}

/// 单点精确抽帧。
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

/// 场景阶段入口：返回保留的 FrameEvent 列表（t 升序）。
pub async fn run(cfg: &PipelineConfig, media: &Path) -> Result<Vec<FrameEvent>> {
    let frames_dir = cfg.frames_dir();
    tokio::fs::create_dir_all(&frames_dir).await?;

    let t0 = std::time::Instant::now();
    let cands = detect_scenes(media, cfg.scene_threshold).await?;
    tracing::info!(
        candidates = cands.len(),
        threshold = cfg.scene_threshold,
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "scene candidates"
    );

    let times = apply_cooldown(&cands, cfg.cooldown);
    tracing::info!(kept = times.len(), cooldown = cfg.cooldown, "scene cooldown");

    let pb = ProgressBar::new(times.len() as u64);
    pb.set_style(
        ProgressStyle::with_template("{spinner:.green} scene {pos}/{len} [{bar:32.cyan/blue}] {msg}")
            .unwrap()
            .progress_chars("##-"),
    );
    let mut kept: Vec<FrameEvent> = vec![];
    let mut last_hash: Option<u64> = None;
    for (i, &t) in times.iter().enumerate() {
        let name = format!("slide_{:04}.jpg", i + 1);
        let path = frames_dir.join(&name);
        extract_frame(media, t, &path).await?;
        let roi = cfg.roi;
        let hash = tokio::task::spawn_blocking(move || dhash_file(&path, roi))
            .await
            .context("hash 线程失败")??;
        if let Some(lh) = last_hash {
            let informative = hash.0 != 0 && lh != 0;
            if informative && hamming(crate::img_hash::DHash(lh), hash) <= cfg.hamming {
                let _ = tokio::fs::remove_file(frames_dir.join(&name)).await;
                pb.inc(1);
                continue;
            }
        }
        last_hash = Some(hash.0);
        kept.push(FrameEvent {
            t,
            image: format!("frames/{name}"),
        });
        pb.set_message(format!("t={t:.1}s"));
        pb.inc(1);
    }
    pb.finish_and_clear();
    // 去重可能淘汰帧导致编号空洞：重命名为连续序号
    let mut frames = vec![];
    for (i, mut ev) in kept.into_iter().enumerate() {
        let new_name = format!("slide_{:04}.jpg", i + 1);
        let old = frames_dir.join(&ev.image);
        let new = frames_dir.join(&new_name);
        if old != new {
            let _ = tokio::fs::rename(&old, &new).await;
        }
        ev.image = format!("frames/{new_name}");
        frames.push(ev);
    }
    tracing::info!(
        frames = frames.len(),
        hamming = cfg.hamming,
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "scene done"
    );
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_meta_output() {
        let s = "frame:10   pts:4000  pts_time:4.000\nlavfi.scene_score=0.6\nframe:20 pts:8000 pts_time:8.5\nlavfi.scene_score=0.9\n";
        let c = parse_candidates(s);
        assert_eq!(c.len(), 2);
        assert!((c[0].t - 4.0).abs() < 1e-9 && (c[0].score - 0.6).abs() < 1e-9);
        assert!((c[1].t - 8.5).abs() < 1e-9);
    }

    #[test]
    fn parse_ignores_garbage() {
        let s = "pts_time:abc\nlavfi.scene_score=1\n";
        assert!(parse_candidates(s).is_empty());
        assert!(parse_candidates("").is_empty());
    }

    #[test]
    fn cooldown_keeps_spaced() {
        let c = [
            Candidate { t: 1.0, score: 0.5 },
            Candidate { t: 2.0, score: 0.5 },
            Candidate { t: 15.0, score: 0.5 },
            Candidate { t: 40.0, score: 0.5 },
            Candidate { t: 44.0, score: 0.5 },
        ];
        let kept = apply_cooldown(&c, 10.0);
        assert_eq!(kept, vec![0.0, 15.0, 40.0]);
    }

    #[test]
    fn cooldown_always_keeps_zero() {
        let kept = apply_cooldown(&[], 10.0);
        assert_eq!(kept, vec![0.0]);
    }
}
