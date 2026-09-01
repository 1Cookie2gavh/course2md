//! 场景检测的合成视频回归测试。
//! 用 ffmpeg 现场生成“色块换页”视频（无需提交 fixture），验证状态机的关键行为：
//! - 换页被检出、时间戳为候选首次出现时间（而非 cooldown 到期时间）
//! - cooldown 期间检测不休眠：跳过的中间页、后续页真实起点仍正确
//!
//! 运行需要 ffmpeg；不存在时跳过。

#![cfg(feature = "integration")]

use std::process::Command;

fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// 生成 20s 1280x720 测试视频（黑/灰/白整屏填充，亮度差异大、SSIM 可区分；
/// 不用 drawtext 以兼容无 freetype 的 ffmpeg 构建）：
/// 0-6.5s 白、6.5-7.5s 黑（模拟过渡页）、7.5-14.5s 灰、14.5-20s 白。
fn make_test_video(path: &std::path::Path) {
    let fill = |color: &str, from: &str, to: &str| {
        format!("drawbox=color={color}:t=fill:enable='between(t,{from},{to})'")
    };
    let filter = format!(
        "{},{},{}",
        fill("black", "6.5", "7.5"),
        fill("gray", "7.5", "14.5"),
        fill("white", "14.5", "20")
    );
    let st = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
        .args(["-i", "color=c=white:s=1280x720:d=20:r=10"])
        .args(["-vf", &filter])
        .arg(path)
        .status()
        .expect("spawn ffmpeg");
    assert!(st.success());
}

#[test]
fn scene_detects_slides_with_true_timestamps() {
    if !have_ffmpeg() {
        eprintln!("skip: ffmpeg not found");
        return;
    }
    let dir = std::env::temp_dir().join(format!("c2m-scene-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let video = dir.join("synthetic.mp4");
    make_test_video(&video);

    let cfg = course2md::config::PipelineConfig {
        url: video.display().to_string(),
        out_dir: dir.clone(),
        out_root: dir.clone(),
        similarity: 0.9,
        sample_interval: 0.5,
        cooldown: 10.0,
        slide_mode: "first".into(),
        stable_secs: 0.0,
        max_height: 1080,
        roi: None,
        threads: 2,
        provider: "cpu".into(),
        max_speech: 20.0,
        formats: vec!["md".into()],
        model_dir: dir.clone(),
        keep_video: true,
        no_download: true,
        llm: Default::default(),
        asr_api: Default::default(),
        asr_model: None,
    };

    let frames = tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(course2md::scene::run(&cfg, &video))
        .expect("scene run");

    // 期望（cooldown=10s、first 模式）：
    //   0s "A" 被发射；6.5s "B" 成为候选、7.5s 被 "C" 替换（候选持续更新，检测无盲区）；
    //   cooldown 在 ~10s 结束时发射的是 "C" 的首次出现时间 7.5s（而非 10s）；
    //   14.5s "D" 距上次发射 <10s，被跳过。
    let ts: Vec<f64> = frames.iter().map(|f| f.t).collect();
    assert!(ts.len() >= 2, "至少应检出 2 帧，got {ts:?}");
    assert!((ts[0] - 0.0).abs() < 1.0, "第一帧应为 0s 附近，got {}", ts[0]);
    assert!(
        (ts[1] - 7.5).abs() < 1.0,
        "第二帧应为 C 页首次出现时间 7.5s（而非 cooldown 到期的 ~10s），got {}（全部：{ts:?}）",
        ts[1]
    );
    let _ = std::fs::remove_dir_all(&dir);
}
