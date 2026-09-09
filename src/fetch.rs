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
    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

/// 抓取元数据（不下载）。
pub async fn fetch_meta(url: &str) -> Result<VideoMeta> {
    let out = run(Command::new("yt-dlp")
        .args(["-J", "--no-warnings", "--no-playlist"])
        .arg(url))
    .await?;
    let meta: VideoMeta = serde_json::from_str(&out).context("解析 yt-dlp 元数据 JSON 失败")?;
    Ok(meta)
}

/// 抓取的平台字幕（yt-dlp 产物）。
pub struct SubtitleFetch {
    pub path: PathBuf,
    /// true = 平台自动生成字幕（auto-caption）
    pub auto: bool,
}

/// 用 yt-dlp 获取平台字幕并转为 srt：先人工字幕，再自动字幕。
/// 平台不提供字幕时 yt-dlp 正常退出但不产出文件 → 返回 None。
pub async fn fetch_subtitle(url: &str, out_dir: &Path) -> Result<Option<SubtitleFetch>> {
    // B 站且有登录态：优先走官方 AI 字幕 API（不走 yt-dlp；yt-dlp 对 B 站 AI 字幕
    // 只能列出、无法真正下载）。失败/无登录态则回落下方 yt-dlp 路径。
    if is_bilibili_video(url) && crate::auth::load_cookie_header().is_some() {
        let u = url.to_string();
        let od = out_dir.to_path_buf();
        match tokio::task::spawn_blocking(move || bilibili_ai_subtitle_srt(&u, &od)).await {
            Ok(Ok(Some(path))) => {
                tracing::info!(path = %path.display(), "使用 B 站 AI 字幕（跳过本地 ASR）");
                return Ok(Some(SubtitleFetch { path, auto: true }));
            }
            Ok(Ok(None)) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "B 站 AI 字幕抓取失败，回落 yt-dlp 路径"),
            Err(_) => {}
        }
    }
    let dir = out_dir.join(".subs");
    // 每次重新抓取，避免读到上次运行残留的旧字幕
    let _ = tokio::fs::remove_dir_all(&dir).await;
    tokio::fs::create_dir_all(&dir).await?;
    let tmpl = dir.join("sub");
    for auto in [false, true] {
        let mut cmd = Command::new("yt-dlp");
        cmd.args([
            "--skip-download",
            // 转为 srt：pick_subtitle_file（subtitle.rs）只认 .srt，两处约定需保持一致
            "--convert-subs",
            "srt",
            "--sub-format",
            "srt/vtt/best",
            "--sub-langs",
            // 语言偏好与 subtitle.rs 的 lang_rank 同源
            crate::subtitle::SUB_LANGS,
            "-o",
        ])
        .arg(&tmpl);
        if auto {
            cmd.arg("--write-auto-subs");
        } else {
            cmd.arg("--write-subs");
        }
        cmd.arg(url);
        // 命令失败（yt-dlp 缺失/网络错误）：记 warn（错误内含 stderr 尾部摘要）后继续尝试 auto；
        // 命令成功但无产物（平台无字幕，yt-dlp 打 warning 后正常退出）不算错误
        if let Err(e) = run(&mut cmd).await {
            tracing::warn!(auto, error = %e, "yt-dlp 字幕抓取失败");
            continue;
        }
        if let Some(path) = crate::subtitle::pick_subtitle_file(&dir) {
            return Ok(Some(SubtitleFetch { path, auto }));
        }
    }
    Ok(None)
}

/// 本地视频的同名字幕 sidecar（lecture.mp4 → lecture.srt/.vtt）。
pub fn sidecar_subtitle(video: &Path) -> Option<SubtitleFetch> {
    crate::subtitle::sidecar_subtitle(video).map(|path| SubtitleFetch { path, auto: false })
}

// ------------------------------------------------------------ B 站 AI 字幕

/// 是否是可直连解析的 B 站视频页（bilibili.com/video/…）。
fn is_bilibili_video(url: &str) -> bool {
    let lower = url.to_lowercase();
    lower.contains("bilibili.com/video/") || lower.contains("b23.tv")
}

/// 从 URL 提取 12 位 BV 号（形如 BV1xxxxxxxxxx）。
fn extract_bvid(url: &str) -> Option<String> {
    let idx = url.find("BV")?;
    let mut out = String::new();
    for c in url[idx..].chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            if out.len() == 12 {
                break;
            }
        } else {
            break;
        }
    }
    if out.len() == 12 && out.starts_with("BV1") {
        Some(out)
    } else {
        None
    }
}

fn bili_http_get(url: &str, cookie: &str) -> Result<serde_json::Value> {
    let resp = ureq::get(url)
        .set(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0 Safari/537.36",
        )
        .set("Referer", "https://www.bilibili.com/")
        .set("Cookie", cookie)
        .timeout(std::time::Duration::from_secs(20))
        .call()
        .with_context(|| format!("B 站接口请求失败: {url}"))?;
    let j: serde_json::Value = resp
        .into_string()
        .with_context(|| "B 站接口响应读取失败")?
        .parse()
        .with_context(|| "B 站接口响应非 JSON")?;
    if j["code"].as_i64() != Some(0) {
        anyhow::bail!(
            "B 站接口返回错误 code={} message={}",
            j["code"],
            j["message"].as_str().unwrap_or("")
        );
    }
    Ok(j)
}

/// 把 AI 字幕 JSON 转成 SRT 文本（subtitle::parse_subtitle 契约：HH:MM:SS,mmm --> …）。
fn ai_subtitle_to_srt(body: &[serde_json::Value]) -> String {
    let mut out = String::new();
    let ts = |sec: f64| -> String {
        let total = (sec.max(0.0) * 1000.0).round() as u64;
        let (h, m, s, ms) = (total / 3_600_000, (total % 3_600_000) / 60_000, (total % 60_000) / 1000, total % 1000);
        format!("{h:02}:{m:02}:{s:02},{ms:03}")
    };
    for (i, item) in body.iter().enumerate() {
        let (Some(from), Some(to)) = (item["from"].as_f64(), item["to"].as_f64()) else {
            continue;
        };
        let text = item["content"].as_str().unwrap_or("").trim().to_string();
        if text.is_empty() {
            continue;
        }
        out.push_str(&format!("{}\n{} --> {}\n{}\n\n", i + 1, ts(from), ts(to), text));
    }
    out
}

/// 走 B 站官方接口抓 AI 字幕并写成 .srt（需要登录态 cookie）。
/// 无字幕 / 失败返回 Ok(None)（调用方回落其它路径）。
fn bilibili_ai_subtitle_srt(url: &str, out_dir: &Path) -> Result<Option<PathBuf>> {
    let Some(cookie) = crate::auth::load_cookie_header() else {
        return Ok(None);
    };
    let Some(bvid) = extract_bvid(url) else {
        return Ok(None);
    };
    // 1) cid（顺带拿分 P 时长，用于字幕覆盖度校验）
    let page = bili_http_get(
        &format!("https://api.bilibili.com/x/player/pagelist?bvid={bvid}&jsonp=jsonp"),
        &cookie,
    )?;
    let Some(first) = page["data"].as_array().and_then(|a| a.first()) else {
        return Ok(None);
    };
    let Some(cid) = first["cid"].as_u64() else {
        return Ok(None);
    };
    let duration_secs = first["duration"].as_f64();
    // 2) 字幕列表：wbi/v2 优先（长视频的 URL 往往只在 wbi/v2 返回），v2 兜底；
    //    subtitle_url 偶发为空 → 两个端点交替重试几次
    let mut chosen: Option<String> = None;
    let eps = [
        "https://api.bilibili.com/x/player/wbi/v2",
        "https://api.bilibili.com/x/player/v2",
    ];
    for _ in 0..6 {
        for ep in eps {
            let v = bili_http_get(&format!("{ep}?bvid={bvid}&cid={cid}"), &cookie)?;
            let subs = v["data"]["subtitle"]["subtitles"].as_array().cloned().unwrap_or_default();
            for lan in ["ai-zh", "ai-en"] {
                if let Some(s) = subs
                    .iter()
                    .find(|s| s["lan"].as_str() == Some(lan) && s["subtitle_url"].as_str().is_some_and(|u| !u.is_empty()))
                {
                    chosen = s["subtitle_url"].as_str().map(|u| u.to_string());
                    break;
                }
            }
            if chosen.is_some() {
                break;
            }
        }
        if chosen.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1500));
    }
    let Some(sub_url) = chosen else {
        return Ok(None);
    };
    // 3) 字幕 JSON。长视频的 AI 字幕可能仍在生成/接口暂态：内容覆盖不足时
    //    间隔递增重试几次（最长约 2 分钟），仍不足才放弃回落其它路径。
    let abs = if sub_url.starts_with("//") {
        format!("https:{sub_url}")
    } else {
        sub_url
    };
    let mut body: Vec<serde_json::Value> = Vec::new();
    for attempt in 0..4 {
        let j: serde_json::Value = ureq::get(&abs)
            .set(
                "User-Agent",
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0 Safari/537.36",
            )
            .set("Referer", "https://www.bilibili.com/")
            .timeout(std::time::Duration::from_secs(25))
            .call()
            .with_context(|| "AI 字幕内容请求失败")?
            .into_string()
            .with_context(|| "AI 字幕响应读取失败")?
            .parse()
            .with_context(|| "AI 字幕响应非 JSON")?;
        body = j["body"].as_array().cloned().unwrap_or_default();
        if body.is_empty() {
            return Ok(None);
        }
        let last_to = body.iter().filter_map(|x| x["to"].as_f64()).fold(0.0_f64, f64::max);
        if subtitle_coverage_ok(last_to, duration_secs) {
            break;
        }
        if attempt < 3 {
            let wait = if attempt == 0 { 15 } else { 30 };
            tracing::warn!(
                last_to,
                cues = body.len(),
                wait_s = wait,
                "B 站 AI 字幕覆盖不足（可能仍在生成），{wait}s 后重试"
            );
            std::thread::sleep(std::time::Duration::from_secs(wait));
        }
    }
    let last_to = body.iter().filter_map(|x| x["to"].as_f64()).fold(0.0_f64, f64::max);
    if !subtitle_coverage_ok(last_to, duration_secs) {
        tracing::warn!(
            last_to,
            duration = ?duration_secs,
            cues = body.len(),
            "B 站 AI 字幕仍未生成完整，弃用并回落其它路径"
        );
        return Ok(None);
    }
    // 4) 写 .srt（与 yt-dlp 产物同目录约定：out/.subs/*.srt）
    let dir = out_dir.join(".subs");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("sub.bilibili-ai.srt");
    let srt = ai_subtitle_to_srt(&body);
    if srt.is_empty() {
        return Ok(None);
    }
    std::fs::write(&path, srt)?;
    Ok(Some(path))
}

/// 字幕覆盖度是否可用：未知时长时要求至少 30 条且覆盖 ≥180s；
/// 已知时长时要求覆盖 ≥60%（不足说明是 B 站未生成完全的残字幕）。
fn subtitle_coverage_ok(last_to: f64, duration_secs: Option<f64>) -> bool {
    match duration_secs {
        Some(dur) if dur > 0.0 => last_to >= dur * 0.6,
        _ => last_to >= 180.0,
    }
}


/// 下载视频到 `dest`（默认 1080p 上限，mp4 合并）。已存在则跳过。
pub async fn download(url: &str, dest: &Path, max_height: u32, verbose: bool) -> Result<()> {
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
        let fmt = format!("bv*[height<={max_height}]+ba/b[height<={max_height}]/b");
        cmd.args([
            "-f",
            &fmt,
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
                // 新版 yt-dlp 在 merge 时会按 --merge-output-format 再补后缀：
                // -o media.mp4.part 实际产出 media.mp4.part.mp4。两种命名都兼容。
                // OsString 拼接而非 format!("{}", display())：非 UTF-8 路径也能正确处理
                let merged = {
                    let mut s = tmp.clone().into_os_string();
                    s.push(".mp4");
                    PathBuf::from(s)
                };
                let produced = if merged.is_file() {
                    merged
                } else if tmp.is_file() {
                    tmp
                } else {
                    anyhow::bail!(
                        "yt-dlp 结束但未找到产物（期望 {} 或 {}）",
                        tmp.display(),
                        merged.display()
                    );
                };
                tokio::fs::rename(&produced, dest).await?;
                return Ok(());
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("yt-dlp 下载失败")))
}

async fn run(cmd: &mut Command) -> Result<String> {
    let out = crate::media::run_cmd(cmd, "yt-dlp").await?;
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

    #[test]
    fn extract_bvid_forms() {
        assert_eq!(
            extract_bvid("https://www.bilibili.com/video/BV1vgbE6JESX?vd_source=x").as_deref(),
            Some("BV1vgbE6JESX")
        );
        assert_eq!(extract_bvid("BV1aKSZBME7V").as_deref(), Some("BV1aKSZBME7V"));
        assert!(extract_bvid("https://youtu.be/dQw4w9WgXcQ").is_none());
        assert!(extract_bvid("https://www.bilibili.com/video/BV1vgb").is_none(), "BV 长度不足");
    }

    #[test]
    fn ai_subtitle_to_srt_contract() {
        let body = serde_json::json!([
            {"from": 0.08, "to": 2.08, "content": "第一句 测试"},
            {"from": 2.08, "to": 4.54, "content": "第二句"},
            {"from": 3.0, "to": 5.0, "content": ""},
        ]);
        let body = body.as_array().unwrap().clone();
        let srt = ai_subtitle_to_srt(&body);
        assert!(srt.contains("00:00:00,080 --> 00:00:02,080\n第一句 测试"), "SRT 时间戳/内容格式须与 subtitle::parse_subtitle 契约一致: {srt}");
        assert!(srt.contains("00:00:02,080 --> 00:00:04,540\n第二句"));
        assert!(!srt.contains("00:00:03,000"), "空内容 cue 应被跳过");
    }

    #[test]
    fn subtitle_coverage_guard() {
        // 全长覆盖 / >60% → 可用
        assert!(subtitle_coverage_ok(416.0, Some(416.0)));
        assert!(subtitle_coverage_ok(300.0, Some(416.0)));
        // B 站超长视频只生成了开头一小段 → 弃用（回落 ASR）
        assert!(!subtitle_coverage_ok(113.0, Some(5906.0)));
        assert!(!subtitle_coverage_ok(476.0, Some(5906.0)));
        // 未知时长兜底：≥180s 才算数
        assert!(subtitle_coverage_ok(200.0, None));
        assert!(!subtitle_coverage_ok(100.0, None));
    }
}
