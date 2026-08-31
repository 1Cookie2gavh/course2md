use std::path::{Path, PathBuf};

/// 运行期管线配置（由 CLI 参数归一而来）。
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    pub url: String,
    pub out_dir: PathBuf,
    pub similarity: f64,
    pub sample_interval: f64,
    pub cooldown: f64,
    pub roi: Option<Roi>,
    pub hamming: u32,
    pub threads: i32,
    pub workers: usize,
    pub provider: String,
    pub precision: String,
    pub vad_threshold: f32,
    pub max_speech: f32,
    pub formats: Vec<String>,
    pub model_dir: PathBuf,
    pub keep_video: bool,
    pub no_download: bool,
}

/// 感兴趣区域；坐标可为像素或比例（0.0-1.0）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Roi {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

impl Roi {
    /// 解析 "x1,y1-x2,y2"，坐标可带 `%` 或为像素。
    pub fn parse(s: &str) -> anyhow::Result<Self> {
        let (a, b) = s
            .split_once('-')
            .ok_or_else(|| anyhow::anyhow!("ROI 格式应为 x1,y1-x2,y2，收到 {s:?}"))?;
        let (x1, y1) = parse_xy(a)?;
        let (x2, y2) = parse_xy(b)?;
        let (x1, x2) = (x1.min(x2), x1.max(x2));
        let (y1, y2) = (y1.min(y2), y1.max(y2));
        if x2 <= x1 || y2 <= y1 {
            anyhow::bail!("ROI 为空矩形: {s:?}");
        }
        Ok(Self { x1, y1, x2, y2 })
    }

    /// 按帧尺寸换算为像素矩形。约定：坐标值 ≤1.0 视为比例，>1.0 视为像素。
    pub fn pixels(&self, w: u32, h: u32) -> (u32, u32, u32, u32) {
        let sx = |v: f64| {
            if v <= 1.0 {
                (v * w as f64).round() as u32
            } else {
                (v.round() as u32).min(w)
            }
        };
        let sy = |v: f64| {
            if v <= 1.0 {
                (v * h as f64).round() as u32
            } else {
                (v.round() as u32).min(h)
            }
        };
        let (x1, x2) = (sx(self.x1).min(w.saturating_sub(1)), sx(self.x2).clamp(1, w));
        let (y1, y2) = (sy(self.y1).min(h.saturating_sub(1)), sy(self.y2).clamp(1, h));
        (x1, y1, x2.max(x1 + 1), y2.max(y1 + 1))
    }
}

fn parse_xy(pair: &str) -> anyhow::Result<(f64, f64)> {
    let (a, b) = pair
        .split_once(',')
        .ok_or_else(|| anyhow::anyhow!("ROI 坐标对格式应为 x,y，收到 {pair:?}"))?;
    Ok((parse_coord(a)?, parse_coord(b)?))
}

fn parse_coord(s: &str) -> anyhow::Result<f64> {
    let s = s.trim();
    if let Some(p) = s.strip_suffix('%') {
        let v: f64 = p.trim().parse()?;
        Ok(v / 100.0)
    } else {
        Ok(s.parse()?)
    }
}

impl PipelineConfig {
    pub fn media_path(&self) -> PathBuf {
        self.out_dir.join("media.mp4")
    }
    pub fn audio_path(&self) -> PathBuf {
        self.out_dir.join("audio.wav")
    }
    pub fn frames_dir(&self) -> PathBuf {
        self.out_dir.join("frames")
    }
    pub fn timeline_path(&self) -> PathBuf {
        self.out_dir.join("timeline.jsonl")
    }
    pub fn meta_path(&self) -> PathBuf {
        self.out_dir.join("meta.json")
    }
}

/// XDG 缓存目录。
pub fn cache_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("XDG_CACHE_HOME") {
        PathBuf::from(d).join("course2md")
    } else {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join(".cache").join("course2md")
    }
}

pub fn model_dir_from(opt: Option<&Path>) -> PathBuf {
    opt.map(|p| p.to_path_buf())
        .unwrap_or_else(|| cache_dir().join("models"))
}

/// 像 URL 或已存在的本地文件才当作输入；否则视为没传参数。
pub fn looks_like_source(s: &str) -> bool {
    let p = Path::new(s);
    if p.is_file() {
        return true;
    }
    let t = s.trim();
    t.starts_with("http://")
        || t.starts_with("https://")
        || t.contains("bilibili.com/")
        || t.contains("youtube.com/")
        || t.contains("youtu.be/")
}

/// 未指定 -o 时：`out/<bvid|yt-id|文件名>/`
pub fn infer_out_dir(source: &str) -> PathBuf {
    PathBuf::from("out").join(infer_slug(source))
}

pub fn infer_slug(source: &str) -> String {
    let p = Path::new(source);
    if p.is_file() {
        return sanitize(p.file_stem().and_then(|s| s.to_str()).unwrap_or("local"));
    }
    if let Some(id) = bvid(source) {
        return id;
    }
    if let Some(id) = youtube_id(source) {
        return format!("yt-{id}");
    }
    sanitize(source)
}

fn bvid(s: &str) -> Option<String> {
    let i = s.find("BV")?;
    let id: String = s[i..].chars().take(12).collect();
    if id.len() >= 6 && id.chars().all(|c| c.is_ascii_alphanumeric()) {
        Some(id)
    } else {
        None
    }
}

fn youtube_id(s: &str) -> Option<String> {
    if let Some(rest) = s.split_once("v=").map(|(_, r)| r) {
        let id = rest.split(['&', '#', '/']).next()?;
        if id.len() >= 6 {
            return Some(id.to_string());
        }
    }
    if let Some(rest) = s.split_once("youtu.be/").map(|(_, r)| r) {
        let id = rest.split(['?', '&', '#', '/']).next()?;
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    None
}

fn sanitize(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let s = s.trim_matches('-');
    let s: String = s.chars().take(60).collect();
    if s.is_empty() {
        "untitled".into()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_gate() {
        assert!(!looks_like_source("course2md"));
        assert!(!looks_like_source("run"));
        assert!(looks_like_source("https://www.bilibili.com/video/BV1pb8o6yE8f"));
        assert!(looks_like_source("https://youtu.be/dQw4w9WgXcQ"));
    }

    #[test]
    fn slug_bilibili() {
        assert_eq!(
            infer_slug("https://www.bilibili.com/video/BV1pb8o6yE8f/?spm=1"),
            "BV1pb8o6yE8f"
        );
    }

    #[test]
    fn slug_youtube() {
        assert_eq!(
            infer_slug("https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=1"),
            "yt-dQw4w9WgXcQ"
        );
        assert_eq!(infer_slug("https://youtu.be/dQw4w9WgXcQ"), "yt-dQw4w9WgXcQ");
    }

    #[test]
    fn roi_percent() {
        let r = Roi::parse("25%,0%-100%,100%").unwrap();
        assert_eq!(r, Roi { x1: 0.25, y1: 0.0, x2: 1.0, y2: 1.0 });
        assert_eq!(r.pixels(1000, 800), (250, 0, 1000, 800));
    }

    #[test]
    fn roi_pixels() {
        let r = Roi::parse("0,400-200,720").unwrap();
        assert_eq!(r.pixels(1000, 800), (0, 400, 200, 720));
    }

    #[test]
    fn roi_bad() {
        assert!(Roi::parse("nonsense").is_err());
        assert!(Roi::parse("1,2-3").is_err());
    }
}
