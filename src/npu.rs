//! Intel NPU 后端（OpenVINO WhisperPipeline）。
//!
//! 在具备 Intel Core Ultra (AI Boost) NPU 的设备上，
//! 通过 OpenVINO GenAI 将 Whisper 模型编译并在 NPU 上高速运行。
//! 通过轻量 Python 伴随进程常驻 127.0.0.1:{port}，
//! course2md 逐 chunk 提交并保存 checkpoint。

use crate::checkpoint::Checkpoint;
use crate::config::PipelineConfig;
use crate::timeline::TranscriptEvent;
use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const NPU_WORKER_SCRIPT: &str = r#"
import http.server
import json
import sys
import os
import io
import wave
import time

try:
    import openvino_genai as ov_genai
    import numpy as np
except ImportError as e:
    sys.stderr.write(f"Error: 缺少 openvino_genai 或 numpy: {e}\n请安装: pip install openvino-genai numpy 或使用 uv\n")
    sys.exit(1)

model_arg = sys.argv[1] if len(sys.argv) > 1 else "dseditor/Qwen3-ASR-1.7B-INT8_OpenVINO"
port = int(sys.argv[2]) if len(sys.argv) > 2 else 29381
device = sys.argv[3] if len(sys.argv) > 3 else "NPU"

model_path = model_arg
if not os.path.isdir(model_path):
    try:
        from huggingface_hub import snapshot_download
        print(f"[NPU] 正在下载/加载 ASR 模型 {model_arg}...", flush=True)
        try:
            model_path = snapshot_download(model_arg)
        except Exception as e_dl:
            if "qwen" in str(model_arg).lower():
                print(f"[NPU] Qwen3-ASR 模型下载受限 ({e_dl})，回退预备的 Whisper Large-v3 Turbo...", flush=True)
                model_arg = "OpenVINO/whisper-large-v3-turbo-int8-ov"
                model_path = snapshot_download(model_arg)
            else:
                raise e_dl
    except Exception as e:
        sys.stderr.write(f"Error 下载模型失败 {model_arg}: {e}
")
        sys.exit(1)

print(f"[NPU] 正在将模型加载/编译至 {device}（首次编译可能需要 1~2 分钟）...", flush=True)
t0 = time.time()
is_qwen = "qwen" in str(model_arg).lower() or "qwen" in str(model_path).lower()

if is_qwen and hasattr(ov_genai, "ASRPipeline"):
    try:
        pipe = ov_genai.ASRPipeline(model_path, device)
        gen_cfg = getattr(ov_genai, "ASRGenerationConfig", lambda: None)()
    except Exception as e_qwen:
        sys.stderr.write(f"[NPU] Qwen3 ASR 加载异常 ({e_qwen})，自动回退 WhisperPipeline
")
        from huggingface_hub import snapshot_download
        fallback_path = snapshot_download("OpenVINO/whisper-large-v3-turbo-int8-ov")
        pipe = ov_genai.WhisperPipeline(fallback_path, device)
        gen_cfg = ov_genai.WhisperGenerationConfig(os.path.join(fallback_path, "generation_config.json"))
        gen_cfg.language = "<|zh|>"
else:
    pipe = ov_genai.WhisperPipeline(model_path, device)
    gen_cfg_path = os.path.join(model_path, "generation_config.json")
    if os.path.isfile(gen_cfg_path):
        gen_cfg = ov_genai.WhisperGenerationConfig(gen_cfg_path)
    else:
        gen_cfg = ov_genai.WhisperGenerationConfig()
    gen_cfg.language = "<|zh|>"

print(f"[NPU] 模型在 {device} 就绪（耗时 {time.time()-t0:.2f}s）", flush=True)

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass

    def do_GET(self):
        if self.path in ("/health", "/v1/health"):
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b"{\"status\":\"ok\"}")
        else:
            self.send_response(404)
            self.end_headers()

    def do_POST(self):
        if self.path in ("/shutdown", "/v1/shutdown", "/exit"):
            self.send_response(200)
            self.end_headers()
            self.wfile.write(b'{"status":"bye"}')
            import threading
            threading.Thread(target=lambda: (time.sleep(0.05), os._exit(0))).start()
            return

        n = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(n)
        try:
            req = json.loads(body)
            wav_path = req.get("path")
            if wav_path and os.path.isfile(wav_path):
                with wave.open(wav_path, "rb") as wf:
                    frames = wf.readframes(wf.getnframes())
                    samples = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0
            else:
                import base64
                b64_data = req.get("input_audio", {}).get("data", "")
                raw = base64.b64decode(b64_data)
                with wave.open(io.BytesIO(raw), "rb") as wf:
                    frames = wf.readframes(wf.getnframes())
                    samples = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0

            res = pipe.generate(samples.tolist(), gen_cfg)
            text = res.texts[0].strip() if res.texts else ""
            resp = json.dumps({"text": text}).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(resp)
        except Exception as e:
            err_resp = json.dumps({"error": str(e)}).encode("utf-8")
            self.send_response(500)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(err_resp)

server = http.server.HTTPServer(("127.0.0.1", port), Handler)
print(f"[NPU] 监听 http://127.0.0.1:{port}", flush=True)
server.serve_forever()
"#;

pub fn resolve_npu_model(raw: Option<&str>) -> String {
    let s = raw.unwrap_or("").trim().to_ascii_lowercase();
    match s.as_str() {
        "whisper" | "turbo" | "whisper-turbo" | "large" => {
            "OpenVINO/whisper-large-v3-turbo-int8-ov".into()
        }
        "tiny" | "whisper-tiny" => "OpenVINO/whisper-tiny-fp16-ov".into(),
        "base" | "whisper-base" => "OpenVINO/whisper-base-fp16-ov".into(),
        "small" | "whisper-small" => "OpenVINO/whisper-small-fp16-ov".into(),
        "qwen3-0.6b" | "0.6b" => "dseditor/Qwen3-ASR-0.6B-INT8_ASYM-OpenVINO".into(),
        // 优先默认推荐 Qwen3-ASR 1.7B
        "qwen3" | "qwen3-1.7b" | "1.7b" | "" => "dseditor/Qwen3-ASR-1.7B-INT8_OpenVINO".into(),
        other => other.to_string(),
    }
}

pub fn run_npu(
    cfg: &PipelineConfig,
    wav: &Path,
    max_speech: f64,
    cp: &mut Checkpoint,
) -> Result<Vec<TranscriptEvent>> {
    let t0 = Instant::now();
    let segs = crate::asr::ffmpeg_vad(wav, max_speech as f32)?;
    tracing::info!(segs = segs.len(), "npu vad");
    if segs.is_empty() {
        tracing::warn!("未检测到语音（VAD 结果为空），跳过识别");
        return Ok(vec![]);
    }

    let model_id = resolve_npu_model(cfg.asr_model.as_deref());
    let port = free_port()?;
    let script_path = write_worker_script(&cfg.out_dir)?;

    tracing::info!(model = %model_id, port, "starting npu worker");
    let mut child = spawn_npu_worker(&script_path, &model_id, port)?;
    let base = format!("http://127.0.0.1:{port}");

    if let Err(e) = wait_ready(&base, Duration::from_secs(300)) {
        let _ = child.kill();
        return Err(e).context("Intel NPU 服务启动超时（首次模型编译可能需要更多时间）");
    }
    tracing::info!(
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "npu ready"
    );

    let tmp = std::env::temp_dir().join(format!("course2md-npu-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);

    let pb = ProgressBar::new(segs.len() as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} npu asr {pos}/{len} [{bar:32.cyan/blue}] {elapsed} {msg}",
        )
        .unwrap()
        .progress_chars("##-"),
    );

    let client = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(120))
        .build();

    let mut err: Option<anyhow::Error> = None;

    for (i, seg) in segs.iter().copied().enumerate() {
        let (start, end) = (seg.start, seg.end);
        if cp.is_done(start, end) {
            pb.inc(1);
            continue;
        }

        let chunk = tmp.join(format!("c{i:04}.wav"));
        if let Err(e) = crate::asr::cut_wav(wav, seg.cut_start, seg.cut_end, &chunk) {
            err = Some(e);
            break;
        }

        let req_body = serde_json::json!({
            "path": chunk.to_string_lossy(),
        });

        match client
            .post(&format!("{base}/audio/transcriptions"))
            .send_json(req_body)
        {
            Ok(resp) => {
                let v: serde_json::Value = resp.into_json()?;
                let text = v["text"].as_str().unwrap_or("").trim().to_string();
                if !text.is_empty() {
                    let sanitized = crate::asr::sanitize_qwen_text(&text);
                    if !sanitized.is_empty() {
                        cp.record(start, end, &sanitized);
                    }
                }
            }
            Err(e) => {
                err = Some(anyhow::anyhow!("NPU 转写请求失败: {e}"));
                let _ = std::fs::remove_file(&chunk);
                break;
            }
        }
        let _ = std::fs::remove_file(&chunk);
        pb.inc(1);
    }

    pb.finish_and_clear();
    // 优雅通知 worker 退出，并终止整个进程组（避免 uv 衍生的孙子进程残留）
    let _ = client
        .post(&format!("{base}/shutdown"))
        .timeout(Duration::from_millis(500))
        .send_json(serde_json::json!({}));
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        unsafe {
            libc::kill(-pid, libc::SIGTERM);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&tmp);

    if let Some(e) = err {
        return Err(e);
    }

    let mut all: Vec<TranscriptEvent> = cp.events().to_vec();
    all.sort_by(|a, b| a.start.partial_cmp(&b.start).unwrap());
    tracing::info!(
        n = all.len(),
        secs = format_args!("{:.1}", t0.elapsed().as_secs_f64()),
        "npu asr done"
    );
    Ok(all)
}

fn write_worker_script(out_dir: &Path) -> Result<PathBuf> {
    let dir = out_dir.join(".workers");
    std::fs::create_dir_all(&dir)?;
    let p = dir.join("npu_worker.py");
    std::fs::write(&p, NPU_WORKER_SCRIPT)?;
    Ok(p)
}

fn spawn_npu_worker(script: &Path, model: &str, port: u16) -> Result<Child> {
    // 优先使用 uv（自动处理隔离环境与依赖），若无则回退系统 python3
    let mut cmd = if which("uv").is_some() {
        let mut c = Command::new("uv");
        c.args([
            "run",
            "--with",
            "openvino-genai",
            "--with",
            "huggingface_hub",
            "--with",
            "numpy",
            "python",
        ]);
        c
    } else if which("python3").is_some() {
        Command::new("python3")
    } else {
        anyhow::bail!("未找到 uv 或 python3，无法启动 Intel NPU 识别后端。请先安装 uv 或 python3");
    };

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.arg(script)
        .arg(model)
        .arg(port.to_string())
        .arg("NPU")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());

    cmd.spawn().context("启动 Intel NPU worker 失败")
}

fn free_port() -> Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

fn wait_ready(base: &str, timeout: Duration) -> Result<()> {
    let t0 = Instant::now();
    let url = format!("{base}/health");
    loop {
        if t0.elapsed() > timeout {
            anyhow::bail!("NPU worker 启动超时");
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

fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|p| p.join(cmd))
        .find(|p| p.is_file())
}
