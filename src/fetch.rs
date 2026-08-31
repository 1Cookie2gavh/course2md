//! yt-dlp 子进程封装：元数据抓取 + 视频下载。

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// 我们关心的元数据字段子集。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoMeta {
    pub title: String,
    #[serde(default)]
    pub uploader: String,
    #[serde(default)]
    pub duration: f64,
    pub webpage_url: String,
    #[serde(default)]
    pub extractor: String,
    #[serde(default)]
    pub id: String,
}

impl VideoMeta {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

/// 抓取元数据（不下载）。
pub async fn fetch_meta(url: &str) -> Result<VideoMeta> {
    let out = run(&mut Command::new("yt-dlp").args([
        "-J",
        "--no-warnings",
        "--no-playlist",
    ])
    .arg(url))
    .await?;
    let meta: VideoMeta =
        serde_json::from_str(&out).context("解析 yt-dlp 元数据 JSON 失败")?;
    Ok(meta)
}

/// 下载视频到 `dest`（720p 上限，mp4 合并）。已存在则跳过。
pub async fn download(url: &str, dest: &Path, verbose: bool) -> Result<()> {
    if dest.is_file() {
        tracing::info!(path = %dest.display(), "media exists, skip download");
        return Ok(());
    }
    if let Some(p) = dest.parent() {
        tokio::fs::create_dir_all(p).await?;
    }
    let tmp: PathBuf = dest.with_extension("mp4.part");
    // 网络类错误重试 2 次
    let mut last_err = None;
    for attempt in 0..3 {
        if attempt > 0 {
            tracing::warn!(attempt, "retry yt-dlp");
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        let mut cmd = Command::new("yt-dlp");
        cmd.args([
            "-f",
            "bv*[height<=720]+ba/b[height<=720]/b",
            "-S",
            "ext:mp4:m4a",
            "--merge-output-format",
            "mp4",
            "--no-playlist",
            "--no-part",
            "-o",
        ])
        .arg(&tmp)
        .arg(url);
        if verbose {
            cmd.arg("-v");
        }
        match run_status(&mut cmd).await {
            Ok(()) => {
                tokio::fs::rename(&tmp, dest).await?;
                return Ok(());
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("yt-dlp 下载失败")))
}

async fn run(cmd: &mut Command) -> Result<String> {
    let out = cmd.output().await.context("启动子进程失败")?;
    if !out.status.success() {
        return Err(crate::error::cmd_error(
            "yt-dlp",
            out.status.code(),
            &String::from_utf8_lossy(&out.stderr),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn run_status(cmd: &mut Command) -> Result<()> {
    let status = cmd.status().await.context("启动子进程失败")?;
    if !status.success() {
        anyhow::bail!(crate::error::cmd_error(
            "yt-dlp",
            status.code(),
            "详见上方 yt-dlp 输出"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn meta_roundtrip() {
        let m = VideoMeta {
            title: "t".into(),
            uploader: "u".into(),
            duration: 1.0,
            webpage_url: "https://x".into(),
            extractor: "generic".into(),
            id: "id".into(),
        };
        let dir = std::env::temp_dir().join("course2md-test-meta");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("meta.json");
        m.save(&p).unwrap();
        let m2 = VideoMeta::load(&p).unwrap();
        assert_eq!(m.title, m2.title);
        assert_eq!(m.duration, 1.0);
    }
}
