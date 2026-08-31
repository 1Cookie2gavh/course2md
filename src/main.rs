use clap::Parser;
use course2md::{cli, config, models, pipeline};

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();
    match cli.command {
        cli::Command::Run {
            url,
            out,
            scene_threshold,
            cooldown,
            roi,
            hamming,
            threads,
            workers,
            provider,
            precision,
            vad_threshold,
            max_speech,
            formats,
            model_dir,
            keep_video,
            no_download,
        } => {
            let cfg = config::PipelineConfig {
                url,
                out_dir: out,
                scene_threshold,
                cooldown,
                roi: roi.map(|s| config::Roi::parse(&s)).transpose()?,
                hamming,
                threads,
                workers,
                provider,
                precision,
                vad_threshold,
                max_speech,
                formats,
                model_dir: config::model_dir_from(model_dir.as_deref()),
                keep_video,
                no_download,
            };
            tokio::runtime::Runtime::new()?.block_on(pipeline::run(&cfg))
        }
        cli::Command::Models { cmd } => match cmd {
            cli::ModelsCmd::Download { size, dir } => {
                let root = config::model_dir_from(dir.as_deref());
                let size = models::ModelSize::parse(&size)?;
                tokio::runtime::Runtime::new()?.block_on(models::download_models(&root, size))
            }
            cli::ModelsCmd::List { dir } => {
                let root = config::model_dir_from(dir.as_deref());
                models::list_models(&root);
                Ok(())
            }
        },
    }
}
