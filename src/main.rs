mod cli;
mod config;
mod error;
mod models;

use clap::Parser;

fn main() -> anyhow::Result<()> {
    let cli = cli::Cli::parse();
    match cli.command {
        cli::Command::Models { cmd } => match cmd {
            cli::ModelsCmd::Download { size, dir } => {
                let root = config::model_dir_from(dir.as_deref());
                let size = models::ModelSize::parse(&size)?;
                tokio::runtime::Runtime::new()?
                    .block_on(models::download_models(&root, size))?;
            }
            cli::ModelsCmd::List { dir } => {
                let root = config::model_dir_from(dir.as_deref());
                models::list_models(&root);
            }
        },
        _ => anyhow::bail!("run 子命令尚未实现（后续提交）"),
    }
    Ok(())
}
