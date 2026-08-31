use clap::Parser;
use course2md::cli::{Cli, Command, ModelsCmd, RunOpts};
use course2md::{config, models, pipeline};
use tracing_subscriber::EnvFilter;

fn init_logging(verbose: u8, quiet: bool) {
    let default = if quiet {
        "error"
    } else if verbose >= 2 {
        "debug"
    } else if verbose == 1 {
        "info"
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

fn run_opts_to_cfg(source: String, opts: RunOpts) -> anyhow::Result<config::PipelineConfig> {
    let out_dir = opts
        .out
        .unwrap_or_else(|| config::infer_out_dir(&source));
    Ok(config::PipelineConfig {
        url: source,
        out_dir,
        scene_threshold: opts.scene_threshold,
        cooldown: opts.cooldown,
        roi: opts.roi.map(|s| config::Roi::parse(&s)).transpose()?,
        hamming: opts.hamming,
        threads: opts.threads,
        workers: opts.workers,
        provider: opts.provider,
        precision: opts.precision,
        vad_threshold: opts.vad_threshold,
        max_speech: opts.max_speech,
        formats: opts.formats,
        model_dir: config::model_dir_from(opts.model_dir.as_deref()),
        keep_video: opts.keep_video,
        no_download: opts.no_download,
    })
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Models { cmd }) => {
            init_logging(0, false);
            match cmd {
                ModelsCmd::Download { size, dir } => {
                    let root = config::model_dir_from(dir.as_deref());
                    let size = models::ModelSize::parse(&size)?;
                    tokio::runtime::Runtime::new()?
                        .block_on(models::download_models(&root, size))?;
                }
                ModelsCmd::List { dir } => {
                    let root = config::model_dir_from(dir.as_deref());
                    models::list_models(&root);
                }
            }
            Ok(())
        }
        None => {
            let source = cli.source.ok_or_else(|| {
                anyhow::anyhow!("请提供 YouTube/Bilibili 链接，或本地视频文件。参见 --help")
            })?;
            init_logging(cli.opts.verbose, cli.opts.quiet);
            let cfg = run_opts_to_cfg(source, cli.opts)?;
            tracing::info!(out = %cfg.out_dir.display(), provider = %cfg.provider, "start");
            tokio::runtime::Runtime::new()?.block_on(pipeline::run(&cfg))
        }
    }
}
