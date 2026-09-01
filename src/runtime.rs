//! 长驻子进程生命周期管理（llama-server / NPU worker）。
//!
//! 统一三件此前散落在 asr.rs / npu.rs 的重复实现：
//! - `ManagedChild`：Drop 保证 kill+wait，任何错误路径（含 `?` 早退）不泄漏进程
//! - `wait_ready`：健康轮询期间监视子进程，秒退立即报错而不是傻等 300s 超时
//! - `which` / `free_port`：单一实现

use anyhow::{Context, Result};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

/// kill-on-drop 的子进程句柄。
pub struct ManagedChild {
    child: Child,
    name: &'static str,
}

impl ManagedChild {
    pub fn spawn(name: &'static str, cmd: &mut Command) -> Result<Self> {
        let child = cmd.spawn().with_context(|| format!("启动 {name} 失败"))?;
        Ok(Self { child, name })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// 取出 piped 的 stderr 句柄（配合 [`drain_stderr`] 使用）。
    pub fn take_stderr(&mut self) -> Option<std::process::ChildStderr> {
        self.child.stderr.take()
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// 非阻塞收割；Some = 已退出。
    fn try_status(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }

    pub fn wait(&mut self) -> std::io::Result<ExitStatus> {
        self.child.wait()
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        // 双 kill 无害（对已退出进程返回 Err 被忽略）
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 后台读取子进程 stderr：避免 pipe 写满阻塞子进程；保留尾部若干行
/// 供失败诊断；verbose(debug) 时逐行转发到 tracing。
/// 返回的尾部缓存与会话同寿命，随时可读取。
pub struct StderrTail {
    lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

const STDERR_TAIL_MAX: usize = 100;

impl StderrTail {
    pub fn tail(&self) -> String {
        self.lines.lock().map(|v| v.join("\n")).unwrap_or_default()
    }
}

impl Clone for StderrTail {
    fn clone(&self) -> Self {
        Self {
            lines: self.lines.clone(),
        }
    }
}

impl Default for StderrTail {
    fn default() -> Self {
        Self {
            lines: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }
}

/// 为一个已 spawn 的 stderr pipe 启动 drain 线程。
pub fn drain_stderr(stderr: std::process::ChildStderr) -> StderrTail {
    use std::io::BufRead;
    let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let shared = lines.clone();
    std::thread::spawn(move || {
        let reader = std::io::BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            tracing::debug!(target: "llama_server", "{line}");
            if let Ok(mut v) = shared.lock()
                && !line.trim().is_empty()
            {
                v.push(line);
                let overflow = v.len().saturating_sub(STDERR_TAIL_MAX);
                if overflow > 0 {
                    v.drain(..overflow);
                }
            }
        }
    });
    StderrTail { lines }
}

/// 轮询 `{base}/health` 直到成功；子进程中途退出立即失败（不等满超时）。
pub fn wait_ready(base: &str, timeout: Duration, child: &mut ManagedChild) -> Result<()> {
    let t0 = Instant::now();
    let url = format!("{base}/health");
    loop {
        if let Some(st) = child.try_status() {
            anyhow::bail!(
                "{} 启动过程中已退出（{st}），详见其 stderr 输出",
                child.name()
            );
        }
        if t0.elapsed() > timeout {
            anyhow::bail!("{} 启动超时（{:.0}s）", child.name(), timeout.as_secs_f64());
        }
        if ureq::get(&url)
            .timeout(Duration::from_secs(2))
            .call()
            .is_ok()
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// 在 PATH 上查找可执行文件（Windows 自动尝试 .exe 后缀）。
pub fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let names: Vec<String> = if cfg!(windows) {
        vec![cmd.to_string(), format!("{cmd}.exe")]
    } else {
        vec![cmd.to_string()]
    };
    std::env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |n| dir.join(n)))
        .find(|p| p.is_file())
}

/// 让 OS 分配一个空闲端口。
pub fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn which_finds_common_tools() {
        // CI 环境保证 cargo 存在；本测试机器必有 shell 基础工具
        #[cfg(unix)]
        let probe = "ls";
        #[cfg(not(unix))]
        let probe = "cmd";
        assert!(which(probe).is_some());
        assert!(which("definitely-not-a-real-binary-xyz").is_none());
    }

    #[test]
    fn free_port_returns_bindable() {
        let p = free_port().unwrap();
        // 立即再绑同一端口不一定成功（TOCTOU），但必须是合法端口值
        assert!(p > 0);
    }
}
