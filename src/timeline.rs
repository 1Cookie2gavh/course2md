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
    /// 展示文本（LLM 润色后的）；未润色时与 raw 相同
    pub text: String,
    /// ASR 原始文本（provenance；润色后保留）
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
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
/// 跨越截图边界的语音段先按边界拆开（文本按时间比例近似分割），
/// 避免整段被错误塞进后一张截图（图文错页）。
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
    let boundaries: Vec<f64> = sections.iter().map(|s| s.t).collect();
    for ev in speech {
        for piece in split_at_boundaries(ev, &boundaries) {
            let mid = (piece.start + piece.end) / 2.0;
            let idx = match sections[..].binary_search_by(|s| s.t.partial_cmp(&mid).unwrap()) {
                Ok(i) => i,
                Err(0) => 0, // 首张截图之前
                Err(i) => i - 1,
            };
            sections[idx].speech.push(piece);
        }
    }
    sections
}

/// 把一条语音事件在截图边界处拆成多段；文本按时间比例在字符维度近似分割。
fn split_at_boundaries(ev: TranscriptEvent, boundaries: &[f64]) -> Vec<TranscriptEvent> {
    let inner: Vec<f64> = boundaries
        .iter()
        .copied()
        .filter(|&b| b > ev.start + 0.3 && b < ev.end - 0.3)
        .collect();
    if inner.is_empty() {
        return vec![ev];
    }
    let mut points = vec![ev.start];
    points.extend(inner);
    points.push(ev.end);
    let total = ev.end - ev.start;
    let chars: Vec<char> = ev.text.chars().collect();
    let total_chars = chars.len();
    // 累计端点法：每段末字符位置 = 按时间比例的累计进度（最后一段固定取到末尾），
    // 数学上保证拆分后字符总数守恒（不丢字、不重复）。
    let mut char_pos = 0usize;
    points
        .windows(2)
        .enumerate()
        .map(|(i, w)| {
            let (s, e) = (w[0], w[1]);
            let end_char = if i + 2 == points.len() {
                total_chars
            } else {
                let cumulative = ((e - ev.start) / total).clamp(0.0, 1.0);
                ((total_chars as f64 * cumulative).round() as usize).clamp(char_pos, total_chars)
            };
            let text: String = chars[char_pos..end_char].iter().collect();
            char_pos = end_char;
            TranscriptEvent {
                start: s,
                end: e,
                text,
                raw: ev.raw.clone(),
            }
        })
        .collect()
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
            raw: None,
        }
    }

    #[test]
    fn merge_assigns_by_midpoint() {
        let frames = vec![frame(0.0), frame(60.0), frame(120.0)];
        let speech = vec![sp(10.0, 20.0), sp(50.0, 70.0), sp(5.0, 8.0)];
        let s = merge(frames, speech);
        // sp(50,70) 跨越 60s 边界：拆成 (50,60)->slide0 与 (60,70)->slide1
        assert_eq!(s[0].speech.len(), 3); // mid=15 + mid=6.5 + 拆分前半
        assert_eq!(s[1].speech.len(), 1); // 拆分后半
        // 拆分后的时间正确
        assert!((s[0].speech[1].end - 60.0).abs() < 1e-6);
        assert!((s[1].speech[0].start - 60.0).abs() < 1e-6);
        assert!(merge(vec![], vec![sp(1.0, 2.0)]).is_empty());
    }

    #[test]
    fn boundary_split_proportional_text() {
        let ev = TranscriptEvent {
            start: 0.0,
            end: 10.0,
            text: "一二三四五六七八九十".into(), // 10 chars
            raw: None,
        };
        let parts = split_at_boundaries(ev, &[5.0]);
        assert_eq!(parts.len(), 2);
        let c0 = parts[0].text.chars().count();
        let c1 = parts[1].text.chars().count();
        assert_eq!(c0 + c1, 10);
        assert_eq!(c0, 5); // 50/50 时长 → 一半字符
        // 边界太靠近端点（<0.3s）不拆
        let ev2 = TranscriptEvent { start: 0.0, end: 1.0, text: "abc".into(), raw: None };
        assert_eq!(split_at_boundaries(ev2, &[0.1]).len(), 1);
    }

    #[test]
    fn split_never_loses_or_duplicates_chars() {
        // 10 字符、3 等分：逐段 round 会得 3+3+3=9（丢字），累计端点法必须守恒
        let ev = TranscriptEvent {
            start: 0.0,
            end: 30.0,
            text: "零一二三四五六七八九".into(),
            raw: None,
        };
        let parts = split_at_boundaries(ev.clone(), &[10.0, 20.0]);
        assert_eq!(parts.len(), 3);
        let joined: String = parts.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(joined, ev.text, "拆分必须字符守恒");

        // 不等分 + 非整比例
        let ev2 = TranscriptEvent {
            start: 0.0,
            end: 10.0,
            text: "零一二三四五六七八九".into(),
            raw: None,
        };
        let cases: [&[f64]; 5] = [
            &[3.0, 4.0],
            &[1.7],
            &[9.9],
            &[0.1, 0.2, 9.8, 9.9],
            &[5.0, 5.5, 6.0, 9.0],
        ];
        for bounds in cases {
            let parts = split_at_boundaries(ev2.clone(), bounds);
            let joined: String = parts.iter().map(|p| p.text.as_str()).collect();
            assert_eq!(joined, ev2.text, "bounds={bounds:?} 必须字符守恒");
        }
    }
}
