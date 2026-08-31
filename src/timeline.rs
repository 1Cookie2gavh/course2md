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
    // 事件按 t 排序输出
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

pub fn read_jsonl(path: &Path) -> Result<(Vec<FrameEvent>, Vec<TranscriptEvent>)> {
    let mut frames = vec![];
    let mut speech = vec![];
    for line in std::fs::read_to_string(path)?.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<TimelineEvent>(line)? {
            TimelineEvent::Frame(f) => frames.push(f),
            TimelineEvent::Speech(s) => speech.push(s),
        }
    }
    Ok((frames, speech))
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
        FrameEvent { t, image: format!("f{t}.jpg") }
    }

    fn sp(start: f64, end: f64) -> TranscriptEvent {
        TranscriptEvent { start, end, text: format!("{start}-{end}") }
    }

    #[test]
    fn merge_basic_assignment() {
        let frames = vec![frame(0.0), frame(60.0), frame(120.0)];
        let speech = vec![sp(10.0, 20.0), sp(50.0, 70.0), sp(119.0, 200.0)];
        let s = merge(frames, speech);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0].speech.len(), 1); // mid=15 → 段0
        assert_eq!(s[1].speech.len(), 1); // mid=60 → 段1（t<=60 最后一个是60）
        assert_eq!(s[2].speech.len(), 1); // mid=159.5 → 段2
    }

    #[test]
    fn merge_speech_before_first_frame() {
        let s = merge(vec![frame(30.0)], vec![sp(5.0, 10.0), sp(40.0, 50.0)]);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].speech.len(), 2);
    }

    #[test]
    fn merge_no_frames() {
        assert!(merge(vec![], vec![sp(1.0, 2.0)]).is_empty());
    }

    #[test]
    fn merge_no_speech() {
        let s = merge(vec![frame(0.0), frame(10.0)], vec![]);
        assert_eq!(s.len(), 2);
        assert!(s[0].speech.is_empty());
    }

    #[test]
    fn merge_midpoint_boundary() {
        // mid 恰等于某帧时间 → 归属该帧
        let s = merge(vec![frame(0.0), frame(50.0)], vec![sp(40.0, 60.0)]);
        assert_eq!(s[1].speech.len(), 1);
    }

    #[test]
    fn jsonl_roundtrip() {
        let dir = std::env::temp_dir().join("course2md-test-timeline");
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("timeline.jsonl");
        let frames = vec![frame(0.0), frame(10.0)];
        let speech = vec![sp(1.0, 2.0)];
        write_jsonl(&p, &frames, &speech).unwrap();
        let (f2, s2) = read_jsonl(&p).unwrap();
        assert_eq!(f2, frames);
        assert_eq!(s2, speech);
    }
}
