use anyhow::{Context, Result};

/// 统一错误别名；各阶段返回 anyhow::Result。
pub type BoxError = anyhow::Error;

/// 子进程失败时附带 stderr 摘要的便捷构造。
pub fn cmd_error(program: &str, code: Option<i32>, stderr: &str) -> anyhow::Error {
    let tail: String = stderr
        .lines()
        .rev()
        .take(5)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n");
    anyhow::anyhow!("{program} failed (code={code:?}):\n{tail}")
}

/// 校验外部工具存在（ffmpeg / yt-dlp）。
pub fn require_cmd(cmd: &str) -> Result<()> {
    if which_sync(cmd).is_none() {
        anyhow::bail!("required command not found: {cmd} (try: brew install {cmd})");
    }
    Ok(())
}

fn which_sync(cmd: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|p| p.join(cmd))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_ffmpeg_ok() {
        assert!(require_cmd("ffmpeg").is_ok());
    }

    #[test]
    fn require_missing_fails() {
        assert!(require_cmd("definitely-not-a-cmd-xyz").is_err());
    }
}
