//! 模型缓存管理与下载（llama.cpp Qwen3-ASR GGUF）。
//!
//! 默认目录：`~/.cache/course2md/models/`（可用 `--model-dir` / `models download --dir` 覆盖）
//!
//! ```text
//! models/
//!   llama-qwen3-1.7b/
//!     Qwen3-ASR-1.7B-Q8_0.gguf
//!     mmproj-Qwen3-ASR-1.7B-Q8_0.gguf
//! ```

use anyhow::{Context, Result};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const HF_GGUF: &str = "https://huggingface.co/ggml-org/Qwen3-ASR-1.7B-GGUF/resolve/main";

#[derive(Debug, Clone)]
pub struct LlamaAsr {
    pub model: PathBuf,
    pub mmproj: PathBuf,
}

pub fn llama_paths(root: &Path) -> LlamaAsr {
    let d = root.join("llama-qwen3-1.7b");
    LlamaAsr {
        model: d.join("Qwen3-ASR-1.7B-Q8_0.gguf"),
        mmproj: d.join("mmproj-Qwen3-ASR-1.7B-Q8_0.gguf"),
    }
}

pub fn llama_ready(root: &Path) -> bool {
    let p = llama_paths(root);
    file_complete(&p.model) && file_complete(&p.mmproj)
}

/// 文件完整性：有 manifest（下载完成时记录的精确字节数）时按字节数校验；
/// 无 manifest 的旧缓存退回 >1MB 启发式。
fn file_complete(path: &Path) -> bool {
    let Ok(md) = fs::metadata(path) else {
        return false;
    };
    if !path.is_file() || md.len() <= 1_000_000 {
        return false;
    }
    let manifest = path.with_extension("manifest.json");
    if let Ok(s) = fs::read_to_string(&manifest)
        && let Ok(v) = serde_json::from_str::<serde_json::Value>(&s)
        && let Some(expected) = v.get("size").and_then(|s| s.as_u64())
    {
        return md.len() == expected;
    }
    true
}

pub fn ensure_llama(root: &Path) -> Result<LlamaAsr> {
    if !llama_ready(root) {
        anyhow::bail!(
            "缺少识别模型，请运行：course2md models download\n目录：{}",
            root.display()
        );
    }
    Ok(llama_paths(root))
}

/// 没有模型就下载；下载过程请保持进程运行。
pub async fn ensure_llama_or_download(root: &Path) -> Result<LlamaAsr> {
    if !llama_ready(root) {
        let (zh, en) = (
            "第一次运行，正在下载识别模型（约 2.4GB），请不要退出。",
            "First run: downloading the ASR model (~2.4GB), please keep this process running.",
        );
        tracing::warn!("{}", crate::i18n::tr(en, zh));
        eprintln!("{}", crate::i18n::tr(en, zh));
        download_models(root).await?;
    }
    Ok(llama_paths(root))
}

/// 下载 llama.cpp Qwen3-ASR GGUF。
pub async fn download_models(root: &Path) -> Result<()> {
    fs::create_dir_all(root)?;
    let p = llama_paths(root);
    download_file(
        &format!("{HF_GGUF}/Qwen3-ASR-1.7B-Q8_0.gguf"),
        &p.model,
        "Qwen3-ASR-1.7B-Q8_0.gguf",
    )
    .await?;
    download_file(
        &format!("{HF_GGUF}/mmproj-Qwen3-ASR-1.7B-Q8_0.gguf"),
        &p.mmproj,
        "mmproj-Qwen3-ASR-1.7B-Q8_0.gguf",
    )
    .await?;
    tracing::info!(path = %root.display(), "models ready");
    Ok(())
}

async fn download_file(url: &str, dest: &Path, label: &str) -> Result<()> {
    if dest.is_file() && file_complete(dest) {
        tracing::info!(label, "skip existing");
        return Ok(());
    }
    if dest.is_file() {
        // 校验不过的残留文件（截断/损坏）直接移除，避免"发现坏了却重下不了"
        tracing::warn!(label, "existing file failed integrity check, re-downloading");
        let _ = fs::remove_file(dest);
        let _ = fs::remove_file(dest.with_extension("manifest.json"));
    }
    if let Some(p) = dest.parent() {
        fs::create_dir_all(p)?;
    }
    let tmp = dest.with_extension("part");
    let url = url.to_string();
    let dest = dest.to_path_buf();
    let label = label.to_string();
    tokio::task::spawn_blocking(move || -> Result<()> {
        tracing::info!(label = %label, url = %url, "download");
        let resp = ureq::get(&url).call().context("请求失败")?;
        let total: u64 = resp
            .header("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let pb = indicatif::ProgressBar::new(total.max(1));
        pb.set_style(
            indicatif::ProgressStyle::with_template(
                "{spinner:.green} {msg} [{bar:32.cyan/blue}] {bytes}/{total_bytes} ({eta})",
            )
            .unwrap()
            .progress_chars("##-"),
        );
        pb.set_message(label.clone());
        let mut reader = resp.into_reader();
        let mut out = fs::File::create(&tmp)?;
        let mut buf = vec![0u8; 1024 * 512];
        let mut done: u64 = 0;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut out, &buf[..n])?;
            done += n as u64;
            pb.set_position(done);
        }
        out.sync_all()?;
        drop(out);
        // 完整性：以服务器 Content-Length 为准（而非"实际收到多少"——截断响应会伪装成功）
        if total > 0 && done != total {
            let _ = fs::remove_file(&tmp);
            anyhow::bail!(
                "下载不完整：期望 {total} 字节，实际收到 {done}（请重试）"
            );
        }
        fs::rename(&tmp, &dest)?;
        // manifest 记录 authoritative Content-Length，供后续启动校验
        let _ = fs::write(
            dest.with_extension("manifest.json"),
            serde_json::json!({"size": if total > 0 { total } else { done }}).to_string(),
        );
        pb.finish_and_clear();
        tracing::info!(label = %label, bytes = done, "downloaded");
        Ok(())
    })
    .await
    .context("下载线程失败")??;
    Ok(())
}

pub fn list_models(root: &Path) {
    let p = llama_paths(root);
    println!("模型目录：{}", root.display());
    println!(
        "  model  {} {}",
        if p.model.is_file() { "OK" } else { "缺" },
        p.model.display()
    );
    println!(
        "  mmproj {} {}",
        if p.mmproj.is_file() { "OK" } else { "缺" },
        p.mmproj.display()
    );
}
