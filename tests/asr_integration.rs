//! 需要本机模型与 llama-server：`cargo test -- --ignored`

use course2md::asr::{self, AsrInput};
use course2md::config::{cache_dir, PipelineConfig};
use course2md::models;

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
        similarity: 0.85,
        sample_interval: 1.0,
        cooldown: 10.0,
        roi: None,
        hamming: 6,
        threads: 4,
        workers: 1,
        provider: "gpu".into(),
        precision: "int8".into(),
        vad_threshold: 0.5,
        max_speech: 20.0,
        formats: vec![],
        model_dir: model_root(),
        keep_video: false,
        no_download: true,
        out_root: std::path::PathBuf::from("/tmp"),
    }
}

#[tokio::test]
#[ignore]
async fn asr_transcribes_zh() {
    let llama = match models::ensure_llama(&model_root()) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("skip: {e}");
            return;
        }
    };
    let events = asr::run(
        &default_cfg(),
        AsrInput {
            wav: fixture("zh_short.wav"),
            model: llama.model,
            mmproj: llama.mmproj,
        },
    )
    .await
    .unwrap();
    let text: String = events.iter().map(|e| e.text.as_str()).collect();
    eprintln!("transcript: {text}");
    assert!(text.contains("软件工程"), "转写应包含「软件工程」: {text}");
}
