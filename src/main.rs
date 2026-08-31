use clap::Parser;
use course2md::cli::{Cli, Command, LlmCmd, ModelsCmd, RunOpts};
use course2md::{config, llm, models, pipeline};
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

/// 配置文件 + CLI 覆盖 -> 生效 LLM 设置。
fn resolve_llm(opts: &RunOpts) -> llm::LlmSettings {
    let mut s = llm::load_config().llm;
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

fn run_opts_to_cfg(source: String, opts: RunOpts) -> anyhow::Result<config::PipelineConfig> {
    let llm = resolve_llm(&opts);
    Ok(config::PipelineConfig {
        url: source,
        out_root: opts.out.clone(),
        out_dir: opts.out,
        similarity: opts.similarity,
        sample_interval: opts.sample_interval,
        cooldown: opts.cooldown,
        roi: opts.roi.map(|s| config::Roi::parse(&s)).transpose()?,
        threads: opts.threads,
        provider: opts.provider,
        max_speech: opts.max_speech,
        formats: opts.formats,
        model_dir: config::model_dir_from(opts.model_dir.as_deref()),
        keep_video: opts.keep_video,
        no_download: opts.no_download,
        llm,
    })
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
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
                        llm::load_config(),
                        base_url,
                        api_key,
                        model,
                        disable_hint,
                    )?;
                    let path = llm::save_config(&cfg)?;
                    println!("已写入并开启：{}", path.display());
                    match llm::test_connection(&cfg.llm) {
                        Ok(()) => println!("连接测试通过。"),
                        Err(e) => eprintln!("连接测试未通过（已保存配置）：{e:#}"),
                    }
                }
                LlmCmd::Status => llm::print_status(&llm::load_config()),
                LlmCmd::Disable => {
                    let mut cfg = llm::load_config();
                    cfg.llm.enabled = false;
                    let path = llm::save_config(&cfg)?;
                    println!("已关闭 LLM 润色（凭据保留）：{}", path.display());
                }
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
            let cfg = run_opts_to_cfg(source, cli.opts)?;
            tracing::info!(out = %cfg.out_dir.display(), provider = %cfg.provider, "start");
            tokio::runtime::Runtime::new()?.block_on(pipeline::run(&cfg))
        }
    }
}
