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

const HF_GGUF: &str =
    "https://huggingface.co/ggml-org/Qwen3-ASR-1.7B-GGUF/resolve/main";

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

pub fn ensure_llama(root: &Path) -> Result<LlamaAsr> {
    let p = llama_paths(root);
    if !p.model.is_file() || fs::metadata(&p.model)?.len() < 1_000_000 {
        anyhow::bail!("缺少 Qwen3-ASR GGUF，请运行 `course2md models download`");
    }
    if !p.mmproj.is_file() || fs::metadata(&p.mmproj)?.len() < 1_000_000 {
        anyhow::bail!("缺少 mmproj GGUF，请运行 `course2md models download`");
    }
    Ok(p)
}

/// 下载 llama.cpp Qwen3-ASR GGUF。
pub async fn download_models(root: &Path, _size: ModelSize) -> Result<()> {
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

fn extract_tar_bz2(tarbz2: &Path, root: &Path, inner: &str, dest_name: &str) -> Result<()> {
    tracing::info!(path = %tarbz2.display(), "extract tar.bz2");
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
        tracing::info!(label, "skip existing");
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
        fs::rename(&tmp, &dest)?;
        pb.finish_and_clear();
        tracing::info!(label = %label, bytes = done, "downloaded");
        Ok(())
    })
    .await
    .context("下载线程失败")??;
    Ok(())
}

pub fn list_models(root: &Path) {
    tracing::info!(path = %root.display(), "model dir");
    let vad = vad_path(root);
    tracing::info!(ok = vad.is_file(), "silero_vad.onnx");
    for (size, name) in [(ModelSize::Q17B, "1.7b"), (ModelSize::Q06B, "0.6b")] {
        let q = qwen3_paths(root, size);
        let files = [
            ("conv_frontend.onnx", q.conv_frontend.is_file()),
            ("encoder.int8.onnx", q.encoder.is_file()),
            ("decoder.int8.onnx", q.decoder.is_file()),
            ("tokenizer/vocab.json", q.tokenizer.join("vocab.json").is_file()),
        ];
        let ok = files.iter().all(|f| f.1);
        tracing::info!(model = name, complete = ok, "qwen3");
        for (n, exists) in files {
            if !exists {
                tracing::warn!(file = n, "missing");
            }
        }
    }
}


