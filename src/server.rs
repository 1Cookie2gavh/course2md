//! 本地 Web 服务（`course2md server`）：守护进程 + 内嵌网页。
//!
//! - 只绑定 127.0.0.1；默认端口 18080，占用自动顺延
//! - 任务基于子进程（复用完整管线），顺序队列（GPU/llama-server 单实例必须串行）
//! - SSE 实时推送任务进度与用户级日志；完成后浏览器通知
//! - 网页内可完成：新建任务（URL/本地路径可混合）、配置编辑、凭据清理（remove）、输出浏览

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::Duration;

/// 把 mpsc 接收端包装成 Read，供 SSE 响应流式输出。
struct ChannelReader {
    rx: mpsc::Receiver<String>,
}

impl Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.rx.recv() {
            Ok(s) => {
                let bytes = s.as_bytes();
                let n = bytes.len().min(buf.len());
                buf[..n].copy_from_slice(&bytes[..n]);
                Ok(n)
            }
            Err(_) => Ok(0),
        }
    }
}

/// 默认监听端口（>10000，避开常见服务）。
pub const DEFAULT_PORT: u16 = 18080;
/// 端口顺延上限。
const PORT_RETRY: u16 = 50;

// ---------------------------------------------------------------- state file

fn state_path() -> PathBuf {
    crate::config::config_dir().join("server.json")
}

#[derive(Clone, Serialize, Deserialize, Default)]
struct ServerState {
    pid: Option<u32>,
    port: Option<u16>,
}

fn save_state(pid: u32, port: u16) -> Result<()> {
    let s = serde_json::to_string_pretty(&ServerState {
        pid: Some(pid),
        port: Some(port),
    })?;
    std::fs::write(state_path(), s)?;
    Ok(())
}

fn load_state() -> Option<ServerState> {
    let p = state_path();
    if !p.is_file() {
        return None;
    }
    serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()
}

fn clear_state() {
    let _ = std::fs::remove_file(state_path());
}

// ---------------------------------------------------------------- tasks

#[derive(Clone, Serialize)]
struct Task {
    id: u64,
    source: String,
    status: String, // queued | running | done | failed
    stage: String,
    progress: u8, // 0-100
    log: Vec<String>,
    out_dir: Option<String>,
}

const MAX_LOG: usize = 600;

struct Manager {
    tasks: Mutex<Vec<Task>>,
    queue: Mutex<std::collections::VecDeque<u64>>,
    cond: Condvar,
    listeners: Mutex<Vec<mpsc::Sender<String>>>,
    next_id: AtomicU64,
    cmd: PathBuf,
}

impl Manager {
    fn new(cmd: PathBuf) -> Arc<Self> {
        Arc::new(Manager {
            tasks: Mutex::new(vec![]),
            queue: Mutex::new(std::collections::VecDeque::new()),
            cond: Condvar::new(),
            listeners: Mutex::new(vec![]),
            next_id: AtomicU64::new(1),
            cmd,
        })
    }

    fn broadcast(&self, data: &str) {
        let listeners = self.listeners.lock().unwrap();
        for tx in listeners.iter() {
            let _ = tx.send(data.to_string());
        }
    }

    fn emit(&self, ev: &str) {
        self.broadcast(&format!("data: {ev}\n\n"));
    }

    fn update<F: FnOnce(&mut Task)>(&self, id: u64, f: F) {        let mut tasks = self.tasks.lock().unwrap();
        if let Some(t) = tasks.iter_mut().find(|t| t.id == id) {
            f(t);
            let ev = serde_json::json!({
                "type": "task", "task": t,
            });
            drop(tasks);
            self.broadcast(&format!("data: {ev}\n\n"));
        }
    }

    fn push(&self, source: &str, out_dir: Option<String>, _provider: &str, _llm: Option<bool>) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let task = Task {
            id,
            source: source.to_string(),
            status: "queued".into(),
            stage: "排队中".into(),
            progress: 0,
            log: vec![format!("[任务 {}] 已创建：{}", id, source)],
            out_dir,
        };
        self.tasks.lock().unwrap().push(task);
        {
            let mut q = self.queue.lock().unwrap();
            q.push_back(id);
        }
        self.cond.notify_one();
        self.emit(&serde_json::json!({"type": "task_created", "id": id}).to_string());
        id
    }
}

// ---------------------------------------------------------------- conversion

fn parse_progress(line: &str) -> Option<(String, u8)> {
    let l = line.to_lowercase();
    if l.contains("asr done") {
        return Some(("识别完成".into(), 60));
    }
    if l.contains("llm summary") {
        return Some(("LLM 总结".into(), 92));
    }
    if l.contains("summary done") {
        return Some(("总结完成".into(), 98));
    }
    if l.contains("download video") || l.contains("下载") {
        return Some(("下载视频".into(), 8));
    }
    if l.contains("extract slides") {
        return Some(("提取画面/音频".into(), 15));
    }
    if l.contains("transcribe") {
        return Some(("语音识别".into(), 30));
    }
    // indicatif progress: "... llm 4/30 ..."
    if let Some(rest) = l.strip_prefix("llm ") {
        let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        let rest2 = &rest[num.len()..];
        if let Some(den_s) = rest2.strip_prefix('/')
            && let Ok(a) = num.parse::<u8>()
            && let Ok(b) = den_s.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse::<u8>()
            && b > 0
        {
            let p = 60 + (a as f32 / b as f32 * 30.0) as u8;
            return Some(("LLM 润色".into(), p.min(90)));
        }
    }
    None
}

fn run_conversion(mgr: &Arc<Manager>, id: u64, _source: &str, args: &[String]) {
    mgr.update(id, |t| {
        t.status = "running".into();
        t.stage = "启动中".into();
    });
    mgr.emit(&serde_json::json!({"type": "task_started", "id": id}).to_string());
    let result = (|| -> Result<()> {
        let mut child = Command::new(&mgr.cmd)
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("启动转换进程失败")?;
        let stderr = child.stderr.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let mgr1 = Arc::clone(mgr);
        let reader = std::thread::spawn(move || {
            let reader = BufReader::new(stderr);
            for line in reader.lines() {
                let line = match line {
                    Ok(l) => l,
                    Err(_) => break,
                };
                let mut new_log: Option<String> = None;
                {
                    let mut tasks = mgr1.tasks.lock().unwrap();
                    if let Some(t) = tasks.iter_mut().find(|t| t.id == id) {
                        t.log.push(line.clone());
                        if t.log.len() > MAX_LOG {
                            let excess = t.log.len() - MAX_LOG;
                            t.log.drain(0..excess);
                        }
                        new_log = Some(t.log.join("\n"));
                    }
                }
                if let Some(log) = new_log {
                    mgr1.broadcast(&format!(
                        "data: {}\n\n",
                        serde_json::json!({"type": "log", "id": id, "log": log})
                    ));
                }
                if let Some((stage, progress)) = parse_progress(&line) {
                    mgr1.update(id, |t| {
                        t.stage = stage;
                        t.progress = progress;
                    });
                }
            }
        });
        // drain stdout too (prevent pipe blocking)
        let mgr2 = Arc::clone(mgr);
        let out_reader = std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let line = line.unwrap_or_default();
                {
                    let mut tasks = mgr2.tasks.lock().unwrap();
                    if let Some(t) = tasks.iter_mut().find(|t| t.id == id) {
                        t.log.push(line);
                        if t.log.len() > MAX_LOG {
                            let excess = t.log.len() - MAX_LOG;
                            t.log.drain(0..excess);
                        }
                    }
                }
            }
        });
        let status = child.wait()?;
        let _ = reader.join();
        let _ = out_reader.join();
        if status.success() {
            Ok(())
        } else {
            bail!("转换进程退出码 {}", status.code().unwrap_or(-1))
        }
    })();
    match result {
        Ok(()) => {
            mgr.update(id, |t| {
                t.status = "done".into();
                t.stage = "完成".into();
                t.progress = 100;
                t.log.push("[任务] 转换完成 ✓".into());
            });
            mgr.emit(&serde_json::json!({"type": "task_done", "id": id}).to_string());
        }
        Err(e) => {
            mgr.update(id, |t| {
                t.status = "failed".into();
                t.stage = "失败".into();
                t.log.push(format!("[任务] 失败：{e:#}"));
            });
            mgr.emit(&serde_json::json!({"type": "task_failed", "id": id, "error": format!("{e:#}")}).to_string());
        }
    }
}

fn worker_loop(mgr: Arc<Manager>) {
    loop {
        let id = {
            let mut q = mgr.queue.lock().unwrap();
            while q.is_empty() {
                q = mgr.cond.wait(q).unwrap();
            }
            q.pop_front().unwrap()
        };
        let (source, out_dir, provider, llm) = {
            let tasks = mgr.tasks.lock().unwrap();
            let t = tasks.iter().find(|t| t.id == id).unwrap();
            (
                t.source.clone(),
                t.out_dir.clone(),
                String::new(),
                None,
            )
        };
        let mut args: Vec<String> = vec![];
        args.push(source.clone());
        if let Some(out) = &out_dir
            && !out.trim().is_empty()
        {
            args.push("-o".into());
            args.push(out.clone());
        }
        if !provider.is_empty() {
            args.push("--provider".into());
            args.push(provider.clone());
        }
        if let Some(llm) = llm {
            args.push(if llm { "--llm".into() } else { "--no-llm".into() });
        }
        run_conversion(&mgr, id, &source, &args);
    }
}

// ---------------------------------------------------------------- http

fn json_response(status: u16, body: String) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    tiny_http::Response::from_data(body.into_bytes())
        .with_status_code(status)
        .with_header("Content-Type: application/json; charset=utf-8".parse::<tiny_http::Header>().unwrap())
}

fn ok_json(v: &serde_json::Value) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    json_response(200, serde_json::to_string(v).unwrap_or_else(|_| "{}".into()))
}

fn not_found() -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    json_response(404, "{\"error\":\"not found\"}".into())
}

fn read_body(request: &mut tiny_http::Request) -> String {
    let mut s = String::new();
    if let Some(len) = request.body_length() {
        let mut buf = vec![0u8; len.min(1 << 20)];
        if let Ok(n) = request.as_reader().read(&mut buf) {
            s = String::from_utf8_lossy(&buf[..n]).into_owned();
        }
    }
    s
}

const HTML: &str = include_str!("server.html");

/// 绑定 127.0.0.1 上的第一个可用端口（>= base），返回实际端口。
pub fn find_free_port(base: u16) -> Option<u16> {
    (base..base + PORT_RETRY).find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
}

/// 前台运行服务（`server run`）。
pub fn run(port: u16) -> Result<()> {
    let addr = format!("127.0.0.1:{port}");
    let server = tiny_http::Server::http(&addr)
        .map_err(|e| anyhow::anyhow!("监听 {addr} 失败: {e}"))?;
    let cmd = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("course2md"));
    let mgr = Manager::new(cmd);
    std::thread::spawn({
        let mgr = Arc::clone(&mgr);
        move || worker_loop(mgr)
    });
    tracing::info!("course2md web 服务已启动: http://{addr}");
    println!("course2md web 服务已启动: http://{addr}  (Ctrl+C 停止)");

    for request in server.incoming_requests() {
        let mgr = Arc::clone(&mgr);
        std::thread::spawn(move || handle(&mgr, request));
    }
    Ok(())
}

fn handle(mgr: &Arc<Manager>, mut request: tiny_http::Request) {
    let url = request.url().to_string();
    let method = request.method().clone();
    let (path, query) = match url.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (url.clone(), String::new()),
    };
    let _ = query;
    match (method, path.as_str()) {
        (tiny_http::Method::Get, "/") => {
            let _ = request.respond(
                tiny_http::Response::from_data(HTML.as_bytes().to_vec())
                    .with_header("Content-Type: text/html; charset=utf-8".parse::<tiny_http::Header>().unwrap()),
            );
        }
        (tiny_http::Method::Get, "/api/health") => {
            let _ = request.respond(ok_json(&serde_json::json!({"ok": true, "version": env!("CARGO_PKG_VERSION")})));
        }
        (tiny_http::Method::Get, "/api/tasks") => {
            let tasks = mgr.tasks.lock().unwrap().clone();
            let _ = request.respond(ok_json(&serde_json::json!({"tasks": tasks})));
        }
        (tiny_http::Method::Get, "/api/events") => {
            let (tx, rx) = mpsc::channel::<String>();
            {
                mgr.listeners.lock().unwrap().push(tx);
            }
            let response = tiny_http::Response::new(
                tiny_http::StatusCode(200),
                vec![
                    "Content-Type: text/event-stream".parse().unwrap(),
                    "Cache-Control: no-cache".parse().unwrap(),
                ],
                ChannelReader { rx },
                None,
                None,
            );
            let _ = request.respond(response);
        }
        (tiny_http::Method::Post, "/api/tasks") => {
            let body = read_body(&mut request);
            let v: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::json!({}));
            let sources: Vec<String> = v
                .get("sources")
                .and_then(|s| s.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            let out_dir = v.get("out").and_then(|x| x.as_str()).map(|s| s.to_string());
            let provider = v.get("provider").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let llm = v.get("llm").and_then(|x| x.as_bool());
            if sources.is_empty() {
                let _ = request.respond(json_response(400, "{\"error\":\"sources 为空\"}".into()));
                return;
            }
            let mut ids = vec![];
            for s in &sources {
                ids.push(mgr.push(s, out_dir.clone(), &provider, llm));
            }
            let _ = request.respond(ok_json(&serde_json::json!({"ids": ids})));
        }
        (tiny_http::Method::Get, "/api/config") => {
            let cfg = crate::settings::load().unwrap_or_default();
            let mask = |k: &str| {
                if k.len() > 8 {
                    format!("{}…{}", &k[..4], &k[k.len() - 4..])
                } else if k.is_empty() {
                    String::new()
                } else {
                    "***".into()
                }
            };
            let _ = request.respond(ok_json(&serde_json::json!({
                "llm": {
                    "enabled": cfg.llm.enabled,
                    "base_url": cfg.llm.base_url,
                    "model": cfg.llm.model,
                    "summarize": cfg.llm.summarize,
                    "api_key": mask(&cfg.llm.api_key),
                },
                "asr_api": {
                    "base_url": cfg.asr_api.base_url,
                    "model": cfg.asr_api.model,
                    "api_key": mask(&cfg.asr_api.api_key),
                },
                "defaults": {
                    "out": cfg.defaults.out.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
                },
                "config_path": crate::settings::config_path().display().to_string(),
            })));
        }
        (tiny_http::Method::Post, "/api/config") => {
            let body = read_body(&mut request);
            let v: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::json!({}));
            let mut cfg = crate::settings::load().unwrap_or_default();
            if let Some(llm) = v.get("llm") {
                if let Some(b) = llm.get("enabled").and_then(|x| x.as_bool()) {
                    cfg.llm.enabled = b;
                }
                if let Some(s) = llm.get("base_url").and_then(|x| x.as_str()) {
                    cfg.llm.base_url = s.to_string();
                }
                if let Some(s) = llm.get("model").and_then(|x| x.as_str()) {
                    cfg.llm.model = s.to_string();
                }
                if let Some(b) = llm.get("summarize").and_then(|x| x.as_bool()) {
                    cfg.llm.summarize = b;
                }
                if let Some(s) = llm.get("api_key").and_then(|x| x.as_str())
                    && !s.is_empty()
                    && !s.starts_with("***")
                    && !s.starts_with("sk-…")
                {
                    cfg.llm.api_key = s.to_string();
                }
            }
            if let Some(asr) = v.get("asr_api") {
                if let Some(s) = asr.get("base_url").and_then(|x| x.as_str()) {
                    cfg.asr_api.base_url = s.to_string();
                }
                if let Some(s) = asr.get("model").and_then(|x| x.as_str()) {
                    cfg.asr_api.model = s.to_string();
                }
                if let Some(s) = asr.get("api_key").and_then(|x| x.as_str())
                    && !s.is_empty()
                    && !s.starts_with("***")
                    && !s.starts_with("sk-…")
                {
                    cfg.asr_api.api_key = s.to_string();
                }
            }
            if let Some(d) = v.get("defaults")
                && let Some(s) = d.get("out").and_then(|x| x.as_str())
            {
                if s.trim().is_empty() {
                    cfg.defaults.out = None;
                } else {
                    cfg.defaults.out = Some(PathBuf::from(s.trim()));
                }
            }
            match crate::settings::save(&cfg) {
                Ok(p) => {
                    let _ = request.respond(ok_json(&serde_json::json!({"ok": true, "path": p.display().to_string()})));
                }
                Err(e) => {
                    let _ = request.respond(json_response(500, format!("{{\"error\":\"{}\"}}", e)));
                }
            }
        }
        (tiny_http::Method::Post, "/api/remove") => {
            let body = read_body(&mut request);
            let v: serde_json::Value = serde_json::from_str(&body).unwrap_or(serde_json::json!({}));
            let with_asr = v.get("asr").and_then(|x| x.as_bool()).unwrap_or(false);
            let mut cfg = crate::settings::load().unwrap_or_default();
            cfg.llm.api_key.clear();
            cfg.llm.base_url.clear();
            cfg.llm.model.clear();
            cfg.llm.enabled = false;
            cfg.llm.summarize = false;
            cfg.llm.prompt = None;
            if with_asr {
                cfg.asr_api.api_key.clear();
            }
            match crate::settings::save(&cfg) {
                Ok(p) => {
                    let _ = request.respond(ok_json(&serde_json::json!({"ok": true, "path": p.display().to_string()})));
                }
                Err(e) => {
                    let _ = request.respond(json_response(500, format!("{{\"error\":\"{}\"}}", e)));
                }
            }
        }
        (tiny_http::Method::Get, "/api/outputs") => {
            let cfg = crate::settings::load().unwrap_or_default();
            let root = cfg
                .defaults
                .out
                .clone()
                .unwrap_or_else(|| PathBuf::from("out"));
            let mut outputs = vec![];
            if root.is_dir() {
                collect_outputs(&root, &mut outputs, 0);
            }
            let _ = request.respond(ok_json(&serde_json::json!({"root": root.display().to_string(), "outputs": outputs})));
        }
        (tiny_http::Method::Get, path) if path.starts_with("/api/output/") => {
            // /api/output/<urlencoded path>/course.md|course.html|summary
            let rel = path.trim_start_matches("/api/output/");
            let cfg = crate::settings::load().unwrap_or_default();
            let root = cfg
                .defaults
                .out
                .clone()
                .unwrap_or_else(|| PathBuf::from("out"));
            let canon_root = root.canonicalize().unwrap_or(root.clone());
            let target = canon_root.join(rel);
            let canon_target = target.canonicalize().unwrap_or(target.clone());
            if !canon_target.starts_with(&canon_root) || !canon_target.is_file() {
                let _ = request.respond(not_found());
                return;
            }
            let body = std::fs::read(&canon_target).unwrap_or_default();
            let mime = if rel.ends_with(".html") {
                "text/html; charset=utf-8"
            } else {
                "text/plain; charset=utf-8"
            };
            let _ = request.respond(
                tiny_http::Response::from_data(body)
                    .with_header(format!("Content-Type: {mime}").parse::<tiny_http::Header>().unwrap()),
            );
        }
        _ => {
            let _ = request.respond(not_found());
        }
    }
}

fn collect_outputs(dir: &Path, out: &mut Vec<serde_json::Value>, depth: usize) {
    if depth > 5 {
        return;
    }
    if dir.join("timeline.jsonl").is_file() {
        let title = dir
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let has_md = dir.join("course.md").is_file();
        let has_html = dir.join("course.html").is_file();
        let has_summary = std::fs::read_to_string(dir.join("course.md"))
            .map(|s| s.contains("视频总结"))
            .unwrap_or(false);
        let summary = std::fs::read_to_string(dir.join("course.md"))
            .ok()
            .map(|s| {
                s.lines()
                    .find(|l| l.starts_with("> "))
                    .map(|l| l.trim_start_matches("> ").to_string())
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        out.push(serde_json::json!({
            "title": title,
            "path": dir.display().to_string(),
            "has_md": has_md,
            "has_html": has_html,
            "has_summary": has_summary,
            "summary": summary,
        }));
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            if e.path().is_dir() {
                collect_outputs(&e.path(), out, depth + 1);
            }
        }
    }
}

// ---------------------------------------------------------------- daemon mgmt

fn pid_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output();
        match out {
            Ok(o) => {
                let s = String::from_utf8_lossy(&o.stdout);
                s.contains(&pid.to_string())
            }
            Err(_) => false,
        }
    }
    #[cfg(not(windows))]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
}

fn kill_pid(pid: u32) -> Result<()> {
    #[cfg(windows)]
    {
        let st = Command::new("taskkill").args(["/PID", &pid.to_string(), "/F"]).status()?;
        if st.success() {
            Ok(())
        } else {
            bail!("taskkill 失败（PID {pid}）")
        }
    }
    #[cfg(not(windows))]
    {
        let st = Command::new("kill").args(["-TERM", &pid.to_string()]).status()?;
        if st.success() {
            Ok(())
        } else {
            bail!("kill 失败（PID {pid}）")
        }
    }
}

/// `server start`：启动后台守护进程（关终端仍可访问）。
pub fn start(base_port: u16) -> Result<u16> {
    let port = find_free_port(base_port).context("找不到可用端口")?;
    let exe = std::env::current_exe().context("获取自身路径失败")?;
    let log_path = crate::config::config_dir().join("server.log");
    if let Some(dir) = log_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let log = std::fs::File::create(&log_path).context("创建日志文件失败")?;
    #[cfg(windows)]
    let child = {
        use std::os::windows::process::CommandExt;
        Command::new(&exe)
            .args(["server", "run", "--port", &port.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone()?))
            .stderr(Stdio::from(log))
            // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP：无窗口、独立进程组，关终端不杀
            .creation_flags(0x08000000 | 0x00000200)
            .spawn()
    };
    #[cfg(not(windows))]
    let child = Command::new(&exe)
        .args(["server", "run", "--port", &port.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .spawn();
    let child = child.context("启动后台服务失败")?;
    // 等健康检查通过（最多 ~10s）
    let mut ready = false;
    for _ in 0..50 {
        if let Ok(resp) = ureq::get(&format!("http://127.0.0.1:{port}/api/health"))
            .timeout(Duration::from_secs(2))
            .call()
            && resp.status() == 200
        {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if !ready {
        // 启动失败：清理
        let _ = kill_pid(child.id());
        bail!("服务启动超时（端口 {port}）");
    }
    save_state(child.id(), port)?;
    println!("course2md server 已启动（后台常驻）: http://127.0.0.1:{port}");
    println!("停止：course2md server stop");
    Ok(port)
}

/// `server stop`：停止后台守护进程。
pub fn stop() -> Result<()> {
    match load_state() {
        Some(s) => match s.pid {
            Some(pid) if pid_alive(pid) => {
                kill_pid(pid)?;
                clear_state();
                println!("已停止 course2md server（PID {pid}）");
                Ok(())
            }
            Some(pid) => {
                clear_state();
                println!("PID {pid} 已不在运行（已清理过期状态）");
                Ok(())
            }
            None => {
                clear_state();
                println!("未找到运行中的服务");
                Ok(())
            }
        },
        None => {
            println!("未找到运行中的服务（无状态文件）");
            Ok(())
        }
    }
}

/// `server status`。
pub fn status() -> Result<()> {
    match load_state() {
        Some(s) => {
            if let (Some(pid), Some(port)) = (s.pid, s.port) {
                let alive = pid_alive(pid);
                let healthy = ureq::get(&format!("http://127.0.0.1:{port}/api/health"))
                    .timeout(Duration::from_secs(2))
                    .call()
                    .map(|r| r.status() == 200)
                    .unwrap_or(false);
                println!(
                    "PID {pid} | 端口 {port} | 进程存活: {alive} | 服务健康: {healthy} | http://127.0.0.1:{port}"
                );
            }
            Ok(())
        }
        None => {
            println!("course2md server 未在运行");
            Ok(())
        }
    }
}


