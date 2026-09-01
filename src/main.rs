use clap::FromArgMatches;
use course2md::cli::{Cli, Command, ConfigCmd, LlmCmd, ModelsCmd, RunOpts};
use course2md::{config, llm, models, pipeline, settings};
use tracing_subscriber::EnvFilter;

fn init_logging(verbose: u8, quiet: bool) {
    let default = if quiet {
        "error"
    } else if verbose >= 2 {
        "debug"
    } else {
        "info"
    };
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| default.into());
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(verbose >= 2)
        .compact()
        .init();
}

/// 默认识别后端：
/// - macOS Apple Silicon: 优先 coreml
/// - Linux: 若存在 Intel NPU (/dev/accel/accel0) 且未安装 llama-server，优先 npu
/// - 其余平台/配置: gpu (llama.cpp)
fn default_provider() -> String {
    if cfg!(apple_native) {
        "coreml".into()
    } else if cfg!(target_os = "linux")
        && std::path::Path::new("/dev/accel/accel0").exists()
        && course2md::error::require_cmd("llama-server").is_err()
    {
        "npu".into()
    } else {
        "gpu".into()
    }
}

/// 配置文件 + CLI 覆盖 -> 生效 LLM 设置。
fn resolve_llm(opts: &RunOpts, file: &settings::ConfigFile) -> llm::LlmSettings {
    let mut s = file.llm.clone();
    if opts.no_llm {
        s.enabled = false;
    } else if opts.llm {
        s.enabled = true;
    }
    if let Some(v) = &opts.llm_base_url {
        s.base_url = v.clone();
    }
    if let Some(v) = &opts.llm_api_key {
        s.api_key = v.clone();
    }
    if let Some(v) = &opts.llm_model {
        s.model = v.clone();
    }
    if let Some(v) = &opts.llm_prompt {
        s.prompt = Some(v.clone());
    }
    if opts.no_llm_hint {
        s.disable_hint = true;
    }
    s
}

/// 优先级：CLI 显式参数 > 配置文件 [defaults] > 内置默认。
fn run_opts_to_cfg(
    source: String,
    opts: &RunOpts,
    file: &settings::ConfigFile,
) -> anyhow::Result<config::PipelineConfig> {
    let d = &file.defaults;
    if let Some(p) = &d.provider {
        anyhow::ensure!(
            matches!(p.as_str(), "coreml" | "gpu" | "cpu" | "api"),
            "配置文件 provider 无效：{p:?}（可选 coreml/gpu/cpu/api）"
        );
    }
    if let Some(m) = &d.slide_mode {
        anyhow::ensure!(
            matches!(m.as_str(), "first" | "stable"),
            "配置文件 slide_mode 无效：{m:?}（可选 first/stable）"
        );
    }
    Ok(config::PipelineConfig {
        url: source,
        out_root: opts
            .out
            .clone()
            .or_else(|| d.out.clone())
            .unwrap_or_else(|| "out".into()),
        out_dir: opts.out.clone().or_else(|| d.out.clone()).unwrap_or_else(|| "out".into()),
        similarity: opts.similarity.or(d.similarity).unwrap_or(0.85),
        sample_interval: opts.sample_interval.or(d.sample_interval).unwrap_or(1.0),
        cooldown: opts.cooldown.or(d.cooldown).unwrap_or(10.0),
        max_height: opts.max_height.or(d.max_height).unwrap_or(1080).clamp(240, 2160),
        slide_mode: match opts.slide_mode.clone() {
            Some(course2md::cli::SlideModeArg::First) => "first".into(),
            Some(course2md::cli::SlideModeArg::Stable) => "stable".into(),
            None => d
                .slide_mode
                .clone()
                .unwrap_or_else(|| "stable".into())
                .to_ascii_lowercase(),
        },
        stable_secs: opts.stable_secs.or(d.stable_secs).unwrap_or(0.8).clamp(0.0, 10.0),
        roi: match &opts.roi {
            Some(s) => Some(config::Roi::parse(s)?),
            None => match &d.roi {
                Some(s) => Some(config::Roi::parse(s)?),
                None => None,
            },
        },
        threads: opts.threads.or(d.threads).unwrap_or(4),
        provider: match opts.provider.clone() {
            Some(p) => match p {
                course2md::cli::ProviderArg::Coreml => "coreml",
                course2md::cli::ProviderArg::Gpu => "gpu",
                course2md::cli::ProviderArg::Cpu => "cpu",
                course2md::cli::ProviderArg::Api => "api",
                course2md::cli::ProviderArg::Npu => "npu",
            }
            .to_string(),
            None => d.provider.clone().unwrap_or_else(default_provider),
        },
        max_speech: opts.max_speech.or(d.max_speech).unwrap_or(20.0),
        formats: opts
            .formats
            .clone()
            .or_else(|| d.formats.clone())
            .unwrap_or_else(|| vec!["md".into(), "html".into()]),
        model_dir: config::model_dir_from(
            opts.model_dir
                .as_deref()
                .or(d.model_dir.as_deref()),
        ),
        keep_video: opts.keep_video || d.keep_video.unwrap_or(false),
        no_download: opts.no_download || d.no_download.unwrap_or(false),
        resume: opts.resume || d.resume.unwrap_or(false),
        llm: resolve_llm(opts, file),
        asr_api: resolve_asr_api(opts, file),
        asr_model: opts.asr_model.clone().or_else(|| d.asr_model.clone()),
    })
}

/// 云端 STT 配置合并：CLI > 配置文件 > 默认（OpenRouter）。
fn resolve_asr_api(opts: &RunOpts, file: &settings::ConfigFile) -> crate::settings::AsrApi {
    let mut a = file.asr_api.clone();
    if let Some(v) = &opts.asr_api_base_url {
        a.base_url = v.clone();
    }
    if let Some(v) = &opts.asr_api_key {
        a.api_key = v.clone();
    }
    if let Some(v) = &opts.asr_api_model {
        a.model = v.clone();
    }
    a
}

fn main() -> anyhow::Result<()> {
    course2md::i18n::init();
    // 帮助文本按 locale 改写后再解析
    let cli = {
        use clap::CommandFactory;
        let mut cmd = Cli::command();
        course2md::i18n::apply_cli(&mut cmd);
        let matches = cmd.get_matches();
        Cli::from_arg_matches(&matches)?
    };
    match cli.command {
        Some(Command::Models { cmd }) => {
            init_logging(0, false);
            match cmd {
                ModelsCmd::Download { dir } => {
                    let root = config::model_dir_from(dir.as_deref());
                    tokio::runtime::Runtime::new()?.block_on(models::download_models(&root))?;
                }
                ModelsCmd::List { dir } => {
                    let root = config::model_dir_from(dir.as_deref());
                    models::list_models(&root);
                }
            }
            Ok(())
        }
        Some(Command::Llm { cmd }) => {
            init_logging(0, false);
            match cmd {
                LlmCmd::Setup {
                    base_url,
                    api_key,
                    model,
                    disable_hint,
                } => {
                    let cfg = llm::setup_interactive(
                        settings::load()?,
                        base_url,
                        api_key,
                        model,
                        disable_hint,
                    )?;
                    let path = settings::save(&cfg)?;
                    println!("已写入并开启：{}", path.display());
                    match llm::test_connection(&cfg.llm) {
                        Ok(()) => println!("连接测试通过。"),
                        Err(e) => eprintln!("连接测试未通过（已保存配置）：{e:#}"),
                    }
                }
                LlmCmd::Status => llm::print_status(&settings::load()?),
                LlmCmd::Disable => {
                    let mut cfg = settings::load()?;
                    cfg.llm.enabled = false;
                    let path = settings::save(&cfg)?;
                    println!("已关闭 LLM 润色（凭据保留）：{}", path.display());
                }
            }
            Ok(())
        }
        Some(Command::Config { cmd }) => {
            init_logging(0, false);
            match cmd {
                ConfigCmd::Init { force } => {
                    let path = settings::config_path();
                    if path.is_file() && !force {
                        anyhow::bail!("配置文件已存在：{}（--force 覆盖）", path.display());
                    }
                    if let Some(dir) = path.parent() {
                        std::fs::create_dir_all(dir)?;
                    }
                    std::fs::write(&path, settings::TEMPLATE)?;
                    println!("已生成配置模板：{}", path.display());
                    println!("按需取消注释并修改；命令行参数优先于此文件。");
                }
                ConfigCmd::Show => settings::print_effective(&settings::load()?),
            }
            Ok(())
        }
        None => {
            let source = match cli.source {
                Some(s) if config::looks_like_source(&s) => s,
                _ => {
                    use clap::CommandFactory;
                    let mut cmd = Cli::command();
                    cmd.print_help()?;
                    std::process::exit(2);
                }
            };
            init_logging(cli.opts.verbose, cli.opts.quiet);
            let file = settings::load()?;
            let cfg = run_opts_to_cfg(source, &cli.opts, &file)?;
            tracing::info!(out = %cfg.out_dir.display(), provider = %cfg.provider, "start");
            tokio::runtime::Runtime::new()?.block_on(pipeline::run(&cfg))
        }
    }
}
