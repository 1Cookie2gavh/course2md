use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "course2md",
    version,
    about = "把网课视频转成带截图的文字稿",
    arg_required_else_help = true,
    args_conflicts_with_subcommands = true,
    after_help = "\
Examples:
  course2md https://www.bilibili.com/video/BV1pb8o6yE8f
  course2md https://youtu.be/dQw4w9WgXcQ
  course2md ./lecture.mp4
  course2md models download
"
)]
pub struct Cli {
    /// 视频链接或本地文件
    pub source: Option<String>,

    #[command(flatten)]
    pub opts: RunOpts,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Args, Clone, Debug)]
pub struct RunOpts {
    /// 输出根目录（其下按 平台/标题/编号 归类）
    #[arg(short, long, default_value = "out")]
    pub out: PathBuf,

    /// 画面变化阈值，越低截图越多
    #[arg(long, default_value_t = 0.85)]
    pub similarity: f64,

    /// 每隔几秒检查一次画面
    #[arg(long, default_value_t = 1.0)]
    pub sample_interval: f64,

    /// 新截图之后至少间隔多少秒
    #[arg(long, default_value_t = 10.0)]
    pub cooldown: f64,

    /// 只比较画面中的区域，如 40%,0%-100%,100%
    #[arg(long)]
    pub roi: Option<String>,

    /// 识别线程数
    #[arg(long, default_value_t = 4)]
    pub threads: i32,

    /// 识别设备：gpu（默认，Metal/CUDA）或 cpu
    #[arg(long, default_value = "gpu")]
    pub provider: String,

    /// 单段语音最长秒数（过长会切分）
    #[arg(long, default_value_t = 20.0)]
    pub max_speech: f32,

    /// 输出格式
    #[arg(long, value_delimiter = ',', default_value = "md,html")]
    pub formats: Vec<String>,

    /// 模型目录（默认 ~/.cache/course2md/models）
    #[arg(long)]
    pub model_dir: Option<PathBuf>,

    /// 保留下载的视频文件（media.mp4）
    #[arg(long)]
    pub keep_video: bool,

    /// 跳过下载（目录里已有视频）
    #[arg(long)]
    pub no_download: bool,

    /// 更详细日志
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// 只显示错误
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
    /// 下载离线识别模型（约 2.4GB）
    Download {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// 查看已下载的模型
    List {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}
