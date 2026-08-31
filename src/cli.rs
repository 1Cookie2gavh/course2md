use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "course2md", version, about = "网课视频 → 截图+转写 的课程文字版")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// 完整管线：下载 → 场景截图 → ASR → 合并 → 渲染
    Run {
        /// 视频 URL，或本地视频文件路径
        url: String,

        /// 输出目录
        #[arg(short, long, default_value = "out")]
        out: PathBuf,

        /// ffmpeg scene 阈值（0-1，越小越敏感）
        #[arg(long, default_value_t = 0.35)]
        scene_threshold: f64,

        /// 两次截图的最小间隔（秒）
        #[arg(long, default_value_t = 10.0)]
        cooldown: f64,

        /// 去重时只比较该矩形区域，如 25%,0%-100%,100% 或 0,400-1280,800
        #[arg(long)]
        roi: Option<String>,

        /// dHash 汉明距离 ≤ 此值视为重复帧（0-64）
        #[arg(long, default_value_t = 6)]
        hamming: u32,

        /// ASR 推理线程数（每 worker 分 threads/workers）
        #[arg(long, default_value_t = 4)]
        threads: i32,

        /// 并行 ASR worker 数（每实例约占 2.6GB；CoreML 建议 1–2）
        #[arg(long, default_value_t = 2)]
        workers: usize,

        /// ASR 后端：mps（Apple GPU，默认）| cpu | coreml
        #[arg(long, default_value = "mps")]
        provider: String,

        /// 权重精度：int8（CPU）| fp32（CoreML/GPU）
        #[arg(long, default_value = "int8")]
        precision: String,

        /// Silero VAD 语音概率阈值
        #[arg(long, default_value_t = 0.5)]
        vad_threshold: f32,

        /// 单个语音段最大时长（秒，超长自动切分）
        #[arg(long, default_value_t = 20.0)]
        max_speech: f32,

        /// 输出格式集合
        #[arg(long, value_delimiter = ',', default_value = "md,html,json")]
        formats: Vec<String>,

        /// 模型根目录
        #[arg(long)]
        model_dir: Option<PathBuf>,

        /// 保留下载的 media.mp4
        #[arg(long, default_value_t = false)]
        keep_video: bool,

        /// 跳过下载（out 下已有 media.mp4 时直接用）
        #[arg(long, default_value_t = false)]
        no_download: bool,
    },
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
        /// 模型规格：1.7b（约2.4GB）或 0.6b（约950MB）
        #[arg(long, default_value = "1.7b")]
        size: String,
        /// 模型根目录
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// 列出缓存中的模型与完整度
    List {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}
