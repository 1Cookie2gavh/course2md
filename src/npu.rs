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
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
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
    sys.stderr.write("Error: 缺少 openvino_genai 或 numpy: " + str(e) + "\n请安装: pip install openvino-genai numpy 或使用 uv\n")
    sys.exit(1)

model_arg = sys.argv[1] if len(sys.argv) > 1 else "dseditor/Qwen3-ASR-1.7B-INT8_OpenVINO"
port = int(sys.argv[2]) if len(sys.argv) > 2 else 29381
device = sys.argv[3] if len(sys.argv) > 3 else "NPU"

model_path = model_arg
if not os.path.isdir(model_path):
    try:
        from huggingface_hub import snapshot_download
        print("[NPU] 正在下载/加载 ASR 模型 " + model_arg + "...", flush=True)
        model_path = snapshot_download(model_arg)
    except Exception as e:
        # 不静默更换模型：请求什么模型就报什么错（换模型须用户显式 --asr-model）
        sys.stderr.write("Error: 模型下载失败 " + model_arg + ": " + str(e) + "\n（可显式指定 --asr-model whisper 使用 Whisper）\n")
        sys.exit(1)

print("[NPU] 正在将模型加载/编译至 " + device + "（首次编译可能需要 1~2 分钟）...", flush=True)
t0 = time.time()
is_qwen = "qwen" in str(model_arg).lower() or "qwen" in str(model_path).lower()

if is_qwen and hasattr(ov_genai, "ASRPipeline"):
    try:
        pipe = ov_genai.ASRPipeline(model_path, device)
        gen_cfg = getattr(ov_genai, "ASRGenerationConfig", lambda: None)()
    except Exception as e_qwen:
        # 加载失败直接报错退出，不回退到 Whisper（静默换模型会让转写来源不可追溯）
        sys.stderr.write("Error: Qwen3 ASR 加载失败: " + str(e_qwen) + "\n（如需 Whisper 请显式指定 --asr-model whisper）\n")
        sys.exit(1)
else:
    pipe = ov_genai.WhisperPipeline(model_path, device)
    gen_cfg_path = os.path.join(model_path, "generation_config.json")
    if os.path.isfile(gen_cfg_path):
        gen_cfg = ov_genai.WhisperGenerationConfig(gen_cfg_path)
    else:
        gen_cfg = ov_genai.WhisperGenerationConfig()
    # 语言由模型自动检测，不强制中文（硬编码 <|zh|> 会把英文课转成中文幻觉输出）

print("[NPU] 模型在 " + device + " 就绪（耗时 " + f"{time.time()-t0:.2f}" + "s）", flush=True)

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        pass

    def do_GET(self):
        if self.path in ("/health", "/v1/health"):
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(b'{"status":"ok"}')
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
print("[NPU] 监听 http://127.0.0.1:" + str(port), flush=True)
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
    let port = crate::runtime::free_port()?;
    let script_path = write_worker_script(&cfg.out_dir)?;

    tracing::info!(model = %model_id, port, "starting npu worker");
    let mut child = spawn_npu_worker(&script_path, &model_id, port)?;
    let base = format!("http://127.0.0.1:{port}");

    // ManagedChild：此后任何 ? 早退都会在 Drop 中终止 worker，不再泄漏进程
    crate::runtime::wait_ready(&base, Duration::from_secs(300), &mut child)
        .context("Intel NPU 服务启动失败/超时（首次模型编译可能需要更多时间）")?;
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
                let sanitized = crate::asr::sanitize_qwen_text(&text);
                // 空结果也记录完成（静音 chunk）；写盘失败不标记完成
                if let Err(e) = cp.record(start, end, &sanitized) {
                    let _ = std::fs::remove_file(&chunk);
                    err = Some(e);
                    break;
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
    child.kill();
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

fn spawn_npu_worker(script: &Path, model: &str, port: u16) -> Result<crate::runtime::ManagedChild> {
    // 优先使用 uv（自动处理隔离环境与依赖），若无则回退系统 python3
    let mut cmd = if crate::runtime::which("uv").is_some() {
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
    } else if crate::runtime::which("python3").is_some() {
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

    crate::runtime::ManagedChild::spawn("NPU worker", &mut cmd)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 内嵌脚本必须能被 Python 解析。0.8.x 曾因 f-string 内嵌字面换行导致
    /// 整个 NPU 后端无法启动（SyntaxError 在编译期拦截，任何路径都跑不到）。
    /// 无 python3 的环境下跳过。
    #[test]
    fn worker_script_is_valid_python() {
        let Some(py) = crate::runtime::which("python3") else {
            eprintln!("skip: python3 not found");
            return;
        };
        let dir = std::env::temp_dir().join(format!("c2m-npu-py-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("npu_worker.py");
        std::fs::write(&script, NPU_WORKER_SCRIPT).unwrap();
        let out = std::process::Command::new(py)
            .arg("-m")
            .arg("py_compile")
            .arg(&script)
            .output()
            .expect("spawn python3");
        assert!(
            out.status.success(),
            "NPU worker 脚本存在语法错误：{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn npu_model_aliases() {
        assert_eq!(
            resolve_npu_model(Some("whisper")),
            "OpenVINO/whisper-large-v3-turbo-int8-ov"
        );
        assert_eq!(
            resolve_npu_model(None),
            "dseditor/Qwen3-ASR-1.7B-INT8_OpenVINO"
        );
        assert_eq!(
            resolve_npu_model(Some("org/custom-model")),
            "org/custom-model"
        );
    }
}
