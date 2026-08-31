//! ffmpeg 子进程封装：音频抽取（16k 单声道 PCM wav）与媒体探测。

use anyhow::Result;
use std::path::Path;
use tokio::process::Command;

/// 抽取 16kHz 单声道 s16 wav。已存在则跳过。
pub async fn extract_audio(media: &Path, dest: &Path) -> Result<()> {
    if dest.is_file() {
        tracing::info!(path = %dest.display(), "audio exists, skip extract");
        return Ok(());
    }
    if let Some(p) = dest.parent() {
        tokio::fs::create_dir_all(p).await?;
    }
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y"])
        .arg("-i")
        .arg(media)
        .args(["-vn", "-ac", "1", "-ar", "16000", "-c:a", "pcm_s16le"])
        .arg(dest)
        .output()
        .await?;
    if !out.status.success() {
        return Err(crate::error::cmd_error(
            "ffmpeg",
            out.status.code(),
            &String::from_utf8_lossy(&out.stderr),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub duration: f64,
}

/// 视频宽高与时长。
pub async fn probe_video(media: &Path) -> Option<VideoInfo> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(media)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    // 常见两行：width,height  与 duration；或一行 width,height,duration
    let s = String::from_utf8_lossy(&out.stdout);
    let mut nums: Vec<f64> = s
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter_map(|t| t.trim().parse().ok())
        .collect();
    if nums.len() < 3 {
        if let Some(d) = probe_duration(media).await {
            nums.push(d);
        }
    }
    if nums.len() < 3 {
        return None;
    }
    Some(VideoInfo {
        width: nums[0] as u32,
        height: nums[1] as u32,
        duration: nums[2],
    })
}

/// 用 ffprobe 拿时长（秒）。失败时返回 None（非致命）。
pub async fn probe_duration(media: &Path) -> Option<f64> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(media)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<f64>()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn probe_missing_file_is_none() {
        assert!(probe_duration(Path::new("/nonexistent.mp4")).await.is_none());
    }
}
