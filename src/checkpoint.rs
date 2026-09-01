//! ASR checkpoint：逐 chunk 追加写 asr.jsonl，重跑时按精确时间边界跳过已完成段。
//!
//! - 每个 chunk 完成即 append + flush（崩溃最多丢当前 chunk）
//! - `(start, end)` 精确匹配（同一音频 → VAD 确定性 → 同分段）
//! - 全部完成后写 `.asr_done` 标记，重跑完全跳过 ASR
//! - `--no-resume` 关闭（LLM 阶段结果不缓存，因为它便宜且可变）

use crate::timeline::TranscriptEvent;
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct Checkpoint {
    path: PathBuf,
    done_path: PathBuf,
    file: Option<std::fs::File>,
    done: HashSet<(u64, u64)>,
    events: Vec<TranscriptEvent>,
}

/// f64 → u64 bit 模式（可哈希；时间来自同一 JSON round-trip，位级一致）
fn key(start: f64, end: f64) -> (u64, u64) {
    (start.to_bits(), end.to_bits())
}

impl Checkpoint {
    /// 打开（或续读）out 目录下的 asr.jsonl。`resume=false` 时忽略既有进度。
    pub fn open(out_dir: &Path, resume: bool) -> Result<Self> {
        let path = out_dir.join("asr.jsonl");
        let done_path = out_dir.join(".asr_done");
        let mut cp = Checkpoint {
            path: path.clone(),
            done_path: done_path.clone(),
            file: None,
            done: HashSet::new(),
            events: vec![],
        };
        if !resume {
            return Ok(cp);
        }
        if done_path.is_file() {
            // 上次已全部完成：直接载入全部事件
            cp.events = Self::load_events(&path)?;
            cp.done = cp.events.iter().map(|e| key(e.start, e.end)).collect();
            tracing::info!(n = cp.events.len(), "asr checkpoint complete, reusing");
            return Ok(cp);
        }
        let partial = Self::load_events(&path)?;
        if !partial.is_empty() {
            tracing::info!(n = partial.len(), "asr checkpoint resumed (partial)");
            cp.done = partial.iter().map(|e| key(e.start, e.end)).collect();
            cp.events = partial;
            // 追加模式打开
            cp.file = Some(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .with_context(|| format!("打开 checkpoint {}", path.display()))?,
            );
        }
        Ok(cp)
    }

    fn load_events(path: &Path) -> Result<Vec<TranscriptEvent>> {
        let mut out = vec![];
        if !path.is_file() {
            return Ok(out);
        }
        for (i, line) in std::fs::read_to_string(path)
            .with_context(|| format!("读取 {}", path.display()))?
            .lines()
            .enumerate()
        {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(line) {
                Ok(ev) => out.push(ev),
                Err(e) => {
                    // 最后一行可能是崩溃时的半截写入：忽略；中间损坏则警告
                    tracing::debug!("checkpoint line {} 解析失败（忽略）：{e}", i + 1);
                }
            }
        }
        Ok(out)
    }

    /// 该 chunk 是否已完成（可跳过）。
    pub fn is_done(&self, start: f64, end: f64) -> bool {
        self.done.contains(&key(start, end))
    }

    /// 已完成的所有事件（含 resume 加载的历史 + 本次新增）。
    pub fn events(&self) -> &[TranscriptEvent] {
        &self.events
    }

    /// 记录一个完成的 chunk（append + flush）。
    pub fn record(&mut self, start: f64, end: f64, text: &str) {
        let ev = TranscriptEvent {
            start,
            end,
            text: text.to_string(),
            raw: None,
        };
        if self.file.is_none() {
            let _ = std::fs::create_dir_all(self.path.parent().unwrap_or(Path::new(".")));
            self.file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
                .ok();
        }
        if let Some(f) = &mut self.file
            && writeln!(f, "{}", serde_json::to_string(&ev).unwrap_or_default()).is_ok()
        {
            let _ = f.flush();
        }
        self.done.insert(key(start, end));
        self.events.push(ev);
    }

    /// 全部完成：写标记（后续重跑直接跳过 ASR）。
    pub fn finish(&mut self) {
        let _ = std::fs::write(&self.done_path, b"done\n");
        self.file = None;
    }
}
