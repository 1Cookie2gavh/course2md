use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "course2md",
    version,
    about = "网课视频 → 截图 + 转写的课程文字版",
    arg_required_else_help = true,
    args_conflicts_with_subcommands = true,
    after_help = "\
Examples:
  course2md https://www.bilibili.com/video/BV1pb8o6yE8f
  course2md https://youtu.be/dQw4w9WgXcQ
  course2md ./lecture.mp4
  course2md ./lecture.mp4 -o notes/lec01
  course2md models download
  course2md models list
"
)]
pub struct Cli {
    /// YouTube / Bilibili 链接，或本地视频文件
    pub source: Option<String>,

    #[command(flatten)]
    pub opts: RunOpts,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Args, Clone, Debug)]
pub struct RunOpts {
    /// 输出目录（默认 out/<视频id或文件名>/）
    #[arg(short, long)]
    pub out: Option<PathBuf>,

    /// SSIM 低于此值视为新幻灯片（与 yt-slide-mark 相同，默认 0.85）
    #[arg(long, default_value_t = 0.85)]
    pub similarity: f64,

    /// 采样间隔（秒），每秒看一帧
    #[arg(long, default_value_t = 1.0)]
    pub sample_interval: f64,

    /// 检出新幻灯片后跳过的秒数
    #[arg(long, default_value_t = 10.0)]
    pub cooldown: f64,

    /// 去重 ROI，如 25%,0%-100%,100% 或 0,400-1280,800
    #[arg(long)]
    pub roi: Option<String>,

    /// dHash 汉明距离 ≤ 此值视为重复帧
    #[arg(long, default_value_t = 6)]
    pub hamming: u32,

    /// ASR 推理线程数
    #[arg(long, default_value_t = 4)]
    pub threads: i32,

    /// 并行 ASR worker 数（ONNX CPU 路径）
    #[arg(long, default_value_t = 2)]
    pub workers: usize,

    /// ASR 后端：mps（Apple GPU）| cpu | coreml
    #[arg(long, default_value = "mps")]
    pub provider: String,

    /// 权重精度：int8 | fp32（仅 ONNX 路径）
    #[arg(long, default_value = "int8")]
    pub precision: String,

    /// Silero VAD 阈值
    #[arg(long, default_value_t = 0.5)]
    pub vad_threshold: f32,

    /// 单个语音段最大时长（秒）
    #[arg(long, default_value_t = 20.0)]
    pub max_speech: f32,

    /// 输出格式
    #[arg(long, value_delimiter = ',', default_value = "md,html,json")]
    pub formats: Vec<String>,

    /// 模型根目录
    #[arg(long)]
    pub model_dir: Option<PathBuf>,

    /// 保留下载的 media.mp4
    #[arg(long)]
    pub keep_video: bool,

    /// 跳过下载（输出目录已有 media.mp4）
    #[arg(long)]
    pub no_download: bool,

    /// 更详细日志（可重复：-v / -vv）
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// 只输出错误
    #[arg(short, long)]
    pub quiet: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// 模型管理
    Models {
        #[command(subcommand)]
        cmd: ModelsCmd,
    },
}

#[derive(Subcommand)]
pub enum ModelsCmd {
    /// 下载模型（Qwen3-ASR + Silero VAD）
    Download {
        /// 1.7b 或 0.6b（ONNX int8）
        #[arg(long, default_value = "1.7b")]
        size: String,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// 列出缓存中的模型
    List {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}
