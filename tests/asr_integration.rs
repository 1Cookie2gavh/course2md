//! 集成测试：需要本地模型与 ffmpeg 的重测试用 #[ignore] 标注，
//! 通过 `cargo test -- --ignored` 显式运行。

use course2md::asr::{self, AsrInput};
use course2md::config::{cache_dir, PipelineConfig};
use course2md::models::{self, ModelSize};

fn model_root() -> std::path::PathBuf {
    cache_dir().join("models")
}

fn fixture(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn default_cfg() -> PipelineConfig {
    PipelineConfig {
        url: String::new(),
        out_dir: std::path::PathBuf::from("/tmp"),
        scene_threshold: 0.35,
        cooldown: 10.0,
        roi: None,
        hamming: 6,
        threads: 4,
        vad_threshold: 0.5,
        max_speech: 20.0,
        formats: vec![],
        model_dir: model_root(),
        keep_video: false,
        no_download: true,
    }
}

/// VAD 冒烟：TTS 生成的 7.6s 中文语音应切出 ≥1 段。
#[tokio::test]
#[ignore]
async fn vad_detects_speech() {
    let root = model_root();
    let vad = models::vad_path(&root);
    if !vad.is_file() {
        eprintln!("skip: 无 VAD 模型");
        return;
    }
    let segs = asr::vad_only(&vad, &fixture("zh_short.wav"), 0.5).unwrap();
    assert!(!segs.is_empty(), "应检测到语音段");
    let total: f64 = segs.iter().map(|s| s.1).sum();
    assert!(total > 3.0, "语音总时长应 >3s，实际 {total}");
    eprintln!("segments: {segs:?}");
}

/// 端到端 ASR 冒烟：zh_short.wav → 应包含关键词「软件工程」。
#[tokio::test]
#[ignore]
async fn asr_transcribes_zh() {
    let root = model_root();
    let (vad, qwen) = match models::ensure_models(&root, ModelSize::Q17B) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("skip: {e}");
            return;
        }
    };
    let cfg = default_cfg();
    let events = asr::run(
        &cfg,
        AsrInput {
            wav: fixture("zh_short.wav"),
            vad_model: vad,
            qwen: qwen,
        },
    )
    .await
    .unwrap();
    let text: String = events
        .iter()
        .map(|e| e.text.as_str())
        .collect::<Vec<_>>()
        .join("");
    eprintln!("transcript: {text}");
    assert!(text.contains("软件工程"), "转写应包含「软件工程」: {text}");
}
