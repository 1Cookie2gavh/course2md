use anyhow::Result;

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

/// 校验外部工具存在。
pub fn require_cmd(cmd: &str) -> Result<()> {
    if crate::runtime::which(cmd).is_none() {
        anyhow::bail!("未找到 {cmd}，请先安装。见 README 安装依赖一节。");
    }
    Ok(())
}
