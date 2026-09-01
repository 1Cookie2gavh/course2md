//! 编排：元数据 → 目录 → 下载 → (截图 ∥ 音频) → 识别 → 渲染。

use crate::asr;
use crate::config::{self, PipelineConfig};
use crate::fetch::{self, VideoMeta};
use crate::media;
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
    if !cfg.provider.eq_ignore_ascii_case("coreml")
        && !cfg.provider.eq_ignore_ascii_case("api")
        && !cfg.provider.eq_ignore_ascii_case("npu")
    {
        crate::error::require_cmd("llama-server")?;
    } else if cfg.provider.eq_ignore_ascii_case("coreml")
        && crate::error::require_cmd("llama-server").is_err()
    {
        // fallback 是 best-effort：提前告知而不是失败后才发现
        tracing::warn!("未找到 llama-server：CoreML 若失败将无法回退到 gpu 后端（best-effort）");
    }

    // LLM 预检：配置错误应在跑完昂贵的下载/识别之前暴露
    if cfg.llm.enabled {
        crate::llm::validate(&cfg.llm)?;
    }

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

    let id = if is_local {
        // 本地文件：stem + 内容指纹短哈希，避免同名不同目录的课件互相覆盖
        let fp = local_fingerprint(local);
        format!("{}-{fp}", sanitize_stem(local))
    } else if meta.id.is_empty() {
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
        fetch::download(&cfg.url, &dest, cfg.max_height, tracing::enabled!(tracing::Level::DEBUG)).await?;
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
    // ASR 可能合法返回空（静音课件）；由后续正常渲染成"无语音"讲义

    tracing::info!(device = %cfg.provider, "transcribe");
    let events = asr::run(&cfg, &cfg.audio_path()).await?;

    // 可选的 LLM 润色（配置已在管线开头校验）
    let events = if cfg.llm.enabled {
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
    let (peak_mb, child_peak_mb) = (peak_rss_mb(0), peak_rss_mb(0));
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
    eprintln!("{}", crate::i18n::tr("──────── course2md done ────────", "──────── course2md 完成 ────────"));
    eprintln!("{}: {}", crate::i18n::tr("Title", "标题"), meta.title);
    eprintln!("{}: {}", crate::i18n::tr("Output dir", "输出目录"), out.display());
    eprintln!();
    eprintln!("{}:", crate::i18n::tr("Documents", "文稿"));
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
    eprintln!("{}: {}/frames/  ({} {})", crate::i18n::tr("Screenshots", "截图"), out.display(), sections.len(), crate::i18n::tr("images", "张"));
    eprintln!("音频：{}", cfg.audio_path().display());
    if is_local {
        eprintln!("{}: {}  ({})", crate::i18n::tr("Video", "视频"), media.display(), crate::i18n::tr("local input, untouched", "本地输入，未改动"));
    } else if cfg.keep_video {
        eprintln!("{}: {}  ({})", crate::i18n::tr("Video", "视频"), media.display(), crate::i18n::tr("kept", "已保留"));
    } else {
        eprintln!("{}: {} (--keep-video)", crate::i18n::tr("Video", "视频"), crate::i18n::tr("deleted", "已删除"));
    }
    eprintln!("{}: {}", crate::i18n::tr("Timeline", "时间线"), cfg.timeline_path().display());
    eprintln!();
    eprintln!(
        "{}: {} {} / {} {} / {} {}",
        crate::i18n::tr("Stats", "统计"),
        sections.len(), crate::i18n::tr("screenshots", "张截图"),
        speech_n, crate::i18n::tr("speech segments", "段语音"),
        chars, crate::i18n::tr("chars", "字")
    );
    eprintln!("{}: {}", crate::i18n::tr("Elapsed", "耗时"), fmt_duration(stats.elapsed_secs));
    match (stats.peak_mb, stats.child_peak_mb) {
        (Some(mb), Some(c)) => eprintln!(
            "{}: {mb:.0} MB (course2md) + {} {c:.0} MB (llama-server/ffmpeg)", crate::i18n::tr("Peak memory", "峰值内存"), crate::i18n::tr("largest child", "最大子进程")
        ),
        (Some(mb), None) => eprintln!("{}: {mb:.0} MB", crate::i18n::tr("Peak memory (process RSS)", "峰值内存（本进程 RSS）")),
        _ => eprintln!("{}: {}", crate::i18n::tr("Peak memory", "峰值内存"), crate::i18n::tr("unavailable", "不可用")),
    }
    eprintln!("{}: {}", crate::i18n::tr("Model dir", "模型目录"), cfg.model_dir.display());
    eprintln!("──────────────────────────────");
    if !cfg.llm.enabled && !cfg.llm.disable_hint {
        crate::llm::write_hint_note(&crate::settings::config_path());
    }
}

fn sanitize_stem(p: &Path) -> String {
    p.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("local")
        .chars()
        .take(40)
        .collect()
}

/// canonical_path + size + mtime 的稳定指纹（8 hex，FNV-1a）。
/// 不用 std DefaultHasher：官方明确不保证跨版本稳定，会破坏 resume/cache 键。
fn local_fingerprint(p: &Path) -> String {
    const FNV_OFFS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut h = FNV_OFFS;
    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(FNV_PRIME);
        }
    };
    feed(
        p.canonicalize()
            .unwrap_or_else(|_| p.to_path_buf())
            .to_string_lossy()
            .as_bytes(),
    );
    if let Ok(md) = std::fs::metadata(p) {
        feed(&md.len().to_le_bytes());
        if let Ok(m) = md.modified()
            && let Ok(d) = m.duration_since(std::time::UNIX_EPOCH)
        {
            feed(&d.as_secs().to_le_bytes());
        }
    }
    format!("{h:016x}")[..8].to_string()
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
#[cfg(unix)]
fn peak_rss_mb(who: libc::c_int) -> Option<f64> {
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
fn peak_rss_mb(_who: i32) -> Option<f64> {
    None
}
