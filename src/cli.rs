use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "course2md",
    version,
    about = "Turn course videos into illustrated notes (markdown/HTML)",
    arg_required_else_help = true,
    args_conflicts_with_subcommands = true,
    after_help = "\
Examples:
  course2md https://www.bilibili.com/video/BV1pb8o6yE8f
  course2md https://youtu.be/dQw4w9WgXcQ
  course2md ./lecture.mp4
  course2md models download
  course2md config init   # 生成配置文件模板
  course2md llm setup     # 配置 LLM 字幕润色
"
)]
pub struct Cli {
    /// Video URL or local file
    pub source: Option<String>,

    #[command(flatten)]
    pub opts: RunOpts,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Args, Clone, Debug)]
pub struct RunOpts {
    /// Output root dir (files go to <platform>/<title>/<id>/)
    #[arg(short, long)]
    pub out: Option<PathBuf>,

    /// Frame-similarity threshold; lower = more screenshots
    #[arg(long)]
    pub similarity: Option<f64>,

    /// Check the frame every N seconds
    #[arg(long)]
    pub sample_interval: Option<f64>,

    /// Minimum seconds between two screenshots
    #[arg(long)]
    pub cooldown: Option<f64>,

    /// Max video height to download (1080 recommended for slides/code)
    #[arg(long)]
    pub max_height: Option<u32>,

    /// Slide emission mode: first | stable (stable waits for animation to settle)
    #[arg(long, value_enum)]
    pub slide_mode: Option<crate::config::SlideMode>,

    /// Seconds a frame must stay unchanged before emitting (stable mode)
    #[arg(long)]
    pub stable_secs: Option<f64>,

    /// Compare only a region, e.g. 40%,0%-100%,100%
    #[arg(long)]
    pub roi: Option<String>,

    /// ASR threads
    #[arg(long)]
    pub threads: Option<i32>,

    /// ASR backend: coreml (Apple Silicon only) / gpu (Metal/CUDA) / cpu / npu (Intel NPU) / api (cloud STT)
    #[arg(long, value_enum)]
    pub provider: Option<crate::config::AsrProvider>,

    /// ASR model: qwen3 (default & recommended Qwen3-ASR 1.7B) | whisper (large-v3-turbo) | tiny | base
    #[arg(long)]
    pub asr_model: Option<String>,

    /// Cloud STT base URL (OpenAI-compatible, e.g. https://openrouter.ai/api/v1)
    #[arg(long)]
    pub asr_api_base_url: Option<String>,

    /// Cloud STT API key (OPENROUTER_API_KEY env is also honored)
    #[arg(long)]
    pub asr_api_key: Option<String>,

    /// Cloud STT model (e.g. qwen/qwen3-asr-flash-2026-02-10)
    #[arg(long)]
    pub asr_api_model: Option<String>,

    /// Transcript source: auto (subtitle first, then ASR) | subtitle | asr
    #[arg(long, value_enum)]
    pub transcript_source: Option<crate::config::TranscriptSource>,

    /// Max seconds per speech segment (longer segments are split)
    #[arg(long)]
    pub max_speech: Option<f32>,

    /// Output formats (md/html/json)
    #[arg(long, value_delimiter = ',', value_enum)]
    pub formats: Option<Vec<crate::config::OutputFormat>>,

    /// Model directory (default ~/.cache/course2md/models)
    #[arg(long)]
    pub model_dir: Option<PathBuf>,

    /// Keep the downloaded media.mp4
    #[arg(long)]
    pub keep_video: bool,

    /// Skip download (video already exists)
    #[arg(long)]
    pub no_download: bool,

    /// Enable LLM transcript polish for this run (overrides config)
    #[arg(long)]
    pub llm: bool,

    /// Disable LLM transcript polish for this run
    #[arg(long, conflicts_with = "llm")]
    pub no_llm: bool,

    /// Override LLM base URL (OpenAI-compatible)
    #[arg(long)]
    pub llm_base_url: Option<String>,

    /// Override LLM API key
    #[arg(long)]
    pub llm_api_key: Option<String>,

    /// Override LLM model name
    #[arg(long)]
    pub llm_model: Option<String>,

    /// Override LLM proofreading prompt
    #[arg(long)]
    pub llm_prompt: Option<String>,

    /// Suppress the end-of-run LLM hint (this run)
    #[arg(long)]
    pub no_llm_hint: bool,

    /// More verbose logging
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Resume from checkpoints in the output dir (skip completed ASR chunks)
    #[arg(long)]
    pub resume: bool,

    /// Ignore existing checkpoints and redo everything
    #[arg(long, conflicts_with = "resume")]
    pub no_resume: bool,

    /// Errors only
    #[arg(short, long)]
    pub quiet: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// Manage local models
    Models {
        #[command(subcommand)]
        cmd: ModelsCmd,
    },
    /// Configure LLM transcript polish
    Llm {
        #[command(subcommand)]
        cmd: LlmCmd,
    },
    /// Diagnose the environment (tools, backends, config, model cache)
    Doctor,
    /// Manage the config file
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
}

#[derive(Subcommand)]
pub enum ModelsCmd {
    /// Download the offline ASR model (~2.4GB)
    Download {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// List downloaded models
    List {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
pub enum LlmCmd {
    /// Interactive setup and enable (missing fields are prompted)
    Setup {
        #[arg(long)]
        base_url: Option<String>,
        #[arg(long)]
        api_key: Option<String>,
        #[arg(long)]
        model: Option<String>,
        /// 同时关闭结束提示
        #[arg(long)]
        disable_hint: bool,
    },
    /// Show current settings (key masked)
    Status,
    /// Disable LLM polish (keep credentials)
    Disable,
}

#[derive(Subcommand)]
pub enum ConfigCmd {
    /// Write a commented config template (refuses to overwrite unless --force)
    Init {
        #[arg(long)]
        force: bool,
    },
    /// Show config path and settings read from the file
    Show,
}
