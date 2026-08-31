//! 模型缓存管理与下载。
//!
//! 布局（`--model-dir`，默认 `~/.cache/course2md/models/`）：
//!
//! ```text
//! models/
//!   silero_vad.onnx
//!   qwen3-1.7b/
//!     conv_frontend.onnx
//!     encoder.int8.onnx
//!     decoder.int8.onnx
//!     tokenizer/{vocab.json, merges.txt, tokenizer_config.json}
//!   qwen3-0.6b/            # 解包自 GitHub release tar.bz2（布局同上）
//! ```

use anyhow::{Context, Result};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

const MODELSCOPE_BASE: &str =
    "https://modelscope.cn/models/zengshuishui/Qwen3-ASR-onnx/resolve/master";
const GITHUB_ASR: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models";

/// 一个 ASR 模型套件所需的四个路径。
#[derive(Debug, Clone)]
pub struct Qwen3Paths {
    pub conv_frontend: PathBuf,
    pub encoder: PathBuf,
    pub decoder: PathBuf,
    pub tokenizer: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ModelSize {
    Q17B,
    Q06B,
}

impl ModelSize {
    pub fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "1.7b" => Ok(Self::Q17B),
            "0.6b" => Ok(Self::Q06B),
            _ => anyhow::bail!("未知模型规格 {s:?}（可选 1.7b / 0.6b）"),
        }
    }
    pub fn subdir(&self) -> &'static str {
        match self {
            Self::Q17B => "qwen3-1.7b",
            Self::Q06B => "qwen3-0.6b",
        }
    }
}

pub fn vad_path(root: &Path) -> PathBuf {
    root.join("silero_vad.onnx")
}

pub fn qwen3_paths(root: &Path, size: ModelSize) -> Qwen3Paths {
    qwen3_paths_prec(root, size, "int8")
}

/// `precision`: "int8" | "fp32"。fp32 仅 1.7B（CoreML/GPU 需要浮点图，int8 会回落到 CPU 且更慢）。
pub fn qwen3_paths_prec(root: &Path, size: ModelSize, precision: &str) -> Qwen3Paths {
    let fp32 = precision.eq_ignore_ascii_case("fp32");
    let d = if fp32 {
        root.join(format!("{}-fp32", size.subdir()))
    } else {
        root.join(size.subdir())
    };
    if fp32 {
        Qwen3Paths {
            conv_frontend: d.join("conv_frontend.onnx"),
            encoder: d.join("encoder.onnx"),
            decoder: d.join("decoder.onnx"),
            tokenizer: d.join("tokenizer"),
        }
    } else {
        Qwen3Paths {
            conv_frontend: d.join("conv_frontend.onnx"),
            encoder: d.join("encoder.int8.onnx"),
            decoder: d.join("decoder.int8.onnx"),
            tokenizer: d.join("tokenizer"),
        }
    }
}

impl Qwen3Paths {
    pub fn missing(&self) -> Vec<PathBuf> {
        let mut v = vec![];
        for p in [&self.conv_frontend, &self.encoder, &self.decoder] {
            if !p.is_file() {
                v.push(p.clone());
            }
        }
        // tokenizer 目录至少要有 vocab.json
        if !self.tokenizer.join("vocab.json").is_file() {
            v.push(self.tokenizer.clone());
        }
        v
    }
}

pub fn ensure_models(root: &Path, size: ModelSize) -> Result<(PathBuf, Qwen3Paths)> {
    ensure_models_prec(root, size, "int8")
}

pub fn ensure_models_prec(root: &Path, size: ModelSize, precision: &str) -> Result<(PathBuf, Qwen3Paths)> {
    let vad = vad_path(root);
    if !vad.is_file() {
        anyhow::bail!("缺少 Silero VAD 模型 {}，请先运行 `course2md models download`", vad.display());
    }
    let q = qwen3_paths_prec(root, size, precision);
    let missing = q.missing();
    if !missing.is_empty() {
        let list: Vec<_> = missing.iter().map(|p| p.display().to_string()).collect();
        anyhow::bail!(
            "Qwen3-ASR 模型不完整，缺少: {}\n请先运行 `course2md models download --size {}`",
            list.join(", "),
            match size { ModelSize::Q17B => "1.7b", ModelSize::Q06B => "0.6b" }
        );
    }
    Ok((vad, q))
}

/// 下载模型（带进度条）。幂等：已存在且非空则跳过。
pub async fn download_models(root: &Path, size: ModelSize) -> Result<()> {
    fs::create_dir_all(root)?;
    let vad = vad_path(root);
    download_file(
        &format!("{GITHUB_ASR}/silero_vad.onnx"),
        &vad,
        "Silero VAD",
    )
    .await?;

    let q = qwen3_paths(root, size);
    fs::create_dir_all(&q.tokenizer)?;
    match size {
        ModelSize::Q17B => {
            for (url, dest, label) in [
                (format!("{MODELSCOPE_BASE}/model_1.7B/conv_frontend.onnx"), q.conv_frontend.clone(), "conv_frontend"),
                (format!("{MODELSCOPE_BASE}/model_1.7B/encoder.int8.onnx"), q.encoder.clone(), "encoder.int8"),
                (format!("{MODELSCOPE_BASE}/model_1.7B/decoder.int8.onnx"), q.decoder.clone(), "decoder.int8"),
            ] {
                download_file(&url, &dest, label).await?;
            }
            for f in ["vocab.json", "merges.txt", "tokenizer_config.json"] {
                let _ = download_file(
                    &format!("{MODELSCOPE_BASE}/tokenizer/{f}"),
                    &q.tokenizer.join(f),
                    f,
                )
                .await;
            }
        }
        ModelSize::Q06B => {
            // GitHub release 提供整包 tar.bz2
            let tmp = std::env::temp_dir().join(format!("course2md-qwen3-0.6b-{}.tar.bz2", std::process::id()));
            download_file(
                &format!("{GITHUB_ASR}/sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25.tar.bz2"),
                &tmp,
                "qwen3-0.6b tar.bz2",
            )
            .await?;
            extract_tar_bz2(&tmp, root, "sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25", "qwen3-0.6b")?;
            let _ = fs::remove_file(&tmp);
        }
    }
    println!("模型就绪：{}", root.display());
    Ok(())
}

fn extract_tar_bz2(tarbz2: &Path, root: &Path, inner: &str, dest_name: &str) -> Result<()> {
    println!("解压 {tarbz2:?} …");
    let f = fs::File::open(tarbz2)?;
    let bz = bzip2_rs::DecoderReader::new(f);
    let dest = root.join(dest_name);
    let mut archive = tar::Archive::new(bz);
    archive.set_overwrite(true);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let rel = path
            .strip_prefix(inner)
            .with_context(|| format!("压缩包内路径异常: {path:?}"))?;
        // 跳过 test_wavs（几十 MB 的测试音频）
        if rel.starts_with("test_wavs") {
            continue;
        }
        let out = dest.join(rel);
        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(&out)?;
        } else {
            if let Some(p) = out.parent() {
                fs::create_dir_all(p)?;
            }
            entry.unpack(&out)?;
        }
    }
    Ok(())
}

async fn download_file(url: &str, dest: &Path, label: &str) -> Result<()> {
    if dest.is_file() && fs::metadata(dest)?.len() > 0 {
        println!("  [跳过] {label}（已存在）");
        return Ok(());
    }
    if let Some(p) = dest.parent() {
        fs::create_dir_all(p)?;
    }
    let tmp = dest.with_extension("part");
    let url = url.to_string();
    let dest = dest.to_path_buf();
    let label = label.to_string();
    // ureq 是阻塞的，放到阻塞线程池
    tokio::task::spawn_blocking(move || -> Result<()> {
        println!("  [下载] {label} ← {url}");
        let resp = ureq::get(&url).call().context("请求失败")?;
        let total: Option<u64> = resp
            .header("content-length")
            .and_then(|v| v.parse().ok());
        let mut reader = resp.into_reader();
        let mut out = fs::File::create(&tmp)?;
        let mut buf = vec![0u8; 1024 * 512];
        let mut done: u64 = 0;
        let mut last_pct = u64::MAX;
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            std::io::Write::write_all(&mut out, &buf[..n])?;
            done += n as u64;
            if let Some(t) = total {
                let pct = done * 100 / t.max(1);
                if pct != last_pct && pct % 5 == 0 {
                    println!("    {label}: {pct}% ({}/{})", human_bytes(done), human_bytes(t));
                    last_pct = pct;
                }
            }
        }
        out.sync_all()?;
        drop(out);
        fs::rename(&tmp, &dest)?;
        println!("    {label}: 完成 ({})", human_bytes(done));
        Ok(())
    })
    .await
    .context("下载线程失败")??;
    Ok(())
}

pub fn human_bytes(n: u64) -> String {
    let f = n as f64;
    if f >= 1e9 {
        format!("{:.2}GB", f / 1e9)
    } else if f >= 1e6 {
        format!("{:.1}MB", f / 1e6)
    } else if f >= 1e3 {
        format!("{:.0}KB", f / 1e3)
    } else {
        format!("{n}B")
    }
}

pub fn list_models(root: &Path) {
    println!("模型根目录: {}", root.display());
    let vad = vad_path(root);
    println!("  silero_vad.onnx : {}", mark(vad.is_file()));
    for (size, name) in [(ModelSize::Q17B, "1.7b"), (ModelSize::Q06B, "0.6b")] {
        let q = qwen3_paths(root, size);
        let files = [
            ("conv_frontend.onnx", q.conv_frontend.is_file()),
            ("encoder.int8.onnx", q.encoder.is_file()),
            ("decoder.int8.onnx", q.decoder.is_file()),
            ("tokenizer/vocab.json", q.tokenizer.join("vocab.json").is_file()),
        ];
        let ok = files.iter().all(|f| f.1);
        println!("  qwen3-{name}     : {}", if ok { "✓ 完整" } else { "✗ 不完整" });
        for (n, exists) in files {
            if !exists {
                println!("      缺 {n}");
            }
        }
    }
}

fn mark(ok: bool) -> &'static str {
    if ok {
        "✓"
    } else {
        "✗ 缺失"
    }
}
