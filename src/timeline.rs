//! 时间线：事件类型、frame/speech 合并成 Section、timeline.jsonl 读写。

use crate::fetch::VideoMeta;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FrameEvent {
    /// 帧对应的视频时间（秒）
    pub t: f64,
    /// 相对输出目录的图片路径，如 "frames/slide_0001.jpg"
    pub image: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranscriptEvent {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// 一张截图 + 展示期间的语音。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Section {
    pub t: f64,
    pub image: String,
    pub speech: Vec<TranscriptEvent>,
}

/// timeline.jsonl 的一行。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TimelineEvent {
    Frame(FrameEvent),
    Speech(TranscriptEvent),
}

/// 合并算法（见 docs/DESIGN.md §3.2）：
/// 每条语音按中点归属「时间 ≤ 中点的最后一张截图」；首张之前的归首张。
pub fn merge(frames: Vec<FrameEvent>, speech: Vec<TranscriptEvent>) -> Vec<Section> {
    if frames.is_empty() {
        return vec![];
    }
    let mut sections: Vec<Section> = frames
        .into_iter()
        .map(|f| Section {
            t: f.t,
            image: f.image,
            speech: vec![],
        })
        .collect();
    // frames 已按 t 升序；二分找最后一个 t <= mid 的段
    for ev in speech {
        let mid = (ev.start + ev.end) / 2.0;
        let idx = match sections[..].binary_search_by(|s| s.t.partial_cmp(&mid).unwrap()) {
            Ok(i) => i,
            Err(0) => 0, // 首张截图之前
            Err(i) => i - 1,
        };
        sections[idx].speech.push(ev);
    }
    sections
}

pub fn write_jsonl(path: &Path, frames: &[FrameEvent], speech: &[TranscriptEvent]) -> Result<()> {
    use std::io::Write;
    let mut events: Vec<TimelineEvent> = vec![];
    events.extend(frames.iter().cloned().map(TimelineEvent::Frame));
    events.extend(speech.iter().cloned().map(TimelineEvent::Speech));
    events.sort_by(|a, b| {
        let (ta, tb) = (time_of(a), time_of(b));
        ta.partial_cmp(&tb).unwrap()
    });
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    for ev in &events {
        serde_json::to_writer(&mut f, ev)?;
        f.write_all(b"\n")?;
    }
    Ok(())
}

fn time_of(e: &TimelineEvent) -> f64 {
    match e {
        TimelineEvent::Frame(f) => f.t,
        TimelineEvent::Speech(s) => s.start,
    }
}

/// 全量结构化输出（structured.json 的主体）。
#[derive(Debug, Serialize)]
pub struct CourseDoc<'a> {
    pub meta: &'a VideoMeta,
    pub sections: &'a [Section],
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(t: f64) -> FrameEvent {
        FrameEvent {
            t,
            image: format!("f{t}.jpg"),
        }
    }

    fn sp(start: f64, end: f64) -> TranscriptEvent {
        TranscriptEvent {
            start,
            end,
            text: format!("{start}-{end}"),
        }
    }

    #[test]
    fn merge_assigns_by_midpoint() {
        let frames = vec![frame(0.0), frame(60.0), frame(120.0)];
        let speech = vec![sp(10.0, 20.0), sp(50.0, 70.0), sp(5.0, 8.0)];
        let s = merge(frames, speech);
        assert_eq!(s[0].speech.len(), 2); // mid=15 + mid=6.5
        assert_eq!(s[1].speech.len(), 1); // mid=60
        assert!(merge(vec![], vec![sp(1.0, 2.0)]).is_empty());
    }
}
