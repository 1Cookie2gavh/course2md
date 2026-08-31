//! macOS arm64：编译并静态链接 Apple 原生 ASR/VAD 模块（speech-swift）。
//! 其他平台或设置 COURSE2MD_NO_APPLE=1 时跳过（course2md 回落 llama.cpp 路径）。

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let skip = std::env::var_os("COURSE2MD_NO_APPLE").is_some();
    println!("cargo:rerun-if-env-changed=COURSE2MD_NO_APPLE");
    println!("cargo:rerun-if-changed=native/apple-asr/Package.swift");
    println!("cargo:rerun-if-changed=native/apple-asr/Sources");

    if target_os != "macos" || target_arch != "aarch64" || skip {
        return;
    }
    if !swiftc_available() {
        println!(
            "cargo:warning=未找到 swiftc（需要 Xcode Command Line Tools），跳过 Apple 原生模块；coreml 后端不可用"
        );
        return;
    }

    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let pkg = manifest.join("native/apple-asr");
    let build_dir = pkg.join(".build/release");

    // 增量：Package.swift 或源码变动才重建（swift build 自身也会增量）。
    let stamp = build_dir.join("libCAppleASR.a");
    if !stamp.is_file() {
        let ok = run(Command::new("swift").args(["build", "-c", "release"]).current_dir(&pkg));
        if !ok {
            println!(
                "cargo:warning=swift build 失败（见上方输出），跳过 Apple 原生模块；coreml 后端不可用"
            );
            return;
        }
    }

    println!("cargo:rustc-cfg=apple_native");
    println!("cargo:rustc-link-search=native={}", build_dir.display());
    // libCAppleASR.a 已包含 speech-swift 及其依赖（MLX 等）的全部对象
    println!("cargo:rustc-link-lib=static=CAppleASR");
    // 框架（对象内嵌 autolink 提示，这里显式列出关键项以保证链接顺序）
    for fw in [
        "Foundation", "CoreML", "Metal", "Accelerate", "CoreFoundation",
        "AVFoundation", "AVFAudio", "AppKit", "CoreAudio", "CryptoKit",
        "NaturalLanguage", "Network", "Security", "CoreGraphics",
    ] {
        println!("cargo:rustc-link-lib=framework={fw}");
    }
    // Swift 运行时 overlay（/usr/lib/swift，macOS 15+ 系统自带）
    println!("cargo:rustc-link-search=native=/usr/lib/swift");
    for dylib in [
        "swiftCore", "swift_Concurrency", "swift_Builtin_float", "swift_errno",
        "swiftAccelerate", "swiftAVFoundation", "swiftCoreAudio", "swiftCoreFoundation",
        "swiftCoreImage", "swiftCoreMIDI", "swiftDarwin", "swiftDispatch", "swiftIOKit",
        "swiftMetal", "swiftNaturalLanguage", "swiftObjectiveC", "swiftObservation",
        "swiftos", "swiftOSLog", "swiftQuartzCore", "swiftRegexBuilder", "swiftsimd",
        "swiftSpatial", "swift_StringProcessing", "swiftUniformTypeIdentifiers", "swiftXPC",
    ] {
        println!("cargo:rustc-link-lib=dylib={dylib}");
    }
    println!("cargo:rustc-link-lib=dylib=c++");
    println!("cargo:rustc-link-lib=dylib=objc");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");

    // MLX 运行时需要 metallib 与可执行文件同目录（mlx.metallib）
    // 注意 .build/release 是指向 out/Products/Release 的符号链接
    let products = build_dir.canonicalize().unwrap_or(build_dir);
    let bundle = products.join("mlx-swift_Cmlx.bundle/Contents/Resources/default.metallib");
    if bundle.is_file() {
        if let Ok(out_dir) = std::env::var("OUT_DIR") {
            // OUT_DIR = <target>/<profile>/build/<pkg>-<hash>/out
            let exe_dir = PathBuf::from(out_dir).join("../../../");
            let dest = exe_dir.join("mlx.metallib");
            if std::fs::copy(&bundle, &dest).is_err() {
                println!("cargo:warning=无法复制 mlx.metallib 到 {}", exe_dir.display());
            }
        }
    } else {
        println!("cargo:warning=未找到 mlx.metallib（{}），CoreML 推理可能失败", bundle.display());
    }
    // Swift 5 语言模式包的兼容钩子 + clang 运行时（___isPlatformVersionAtLeast 等）
    if let Some(swift_lib) = toolchain_swift_lib_dir() {
        println!("cargo:rustc-link-search=native={}", swift_lib.display());
        println!("cargo:rustc-link-lib=static=swiftCompatibility56");
    }
    if let Some(rt) = find_clang_rt_osx() {
        println!("cargo:rustc-link-search=native={}", rt.1.display());
        println!("cargo:rustc-link-lib=static=clang_rt.osx");
    }
}

/// toolchain 的 usr/lib/swift/macosx 目录。
fn toolchain_swift_lib_dir() -> Option<PathBuf> {
    let out = Command::new("xcode-select").arg("-p").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let devdir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    if let Ok(rd) = std::fs::read_dir(devdir.join("Toolchains")) {
        for e in rd.flatten() {
            let d = e.path().join("usr/lib/swift/macosx");
            if d.join("libswiftCompatibility56.a").is_file() {
                return Some(d);
            }
        }
    }
    None
}

/// 定位 toolchain 内的 libclang_rt.osx.a，返回 (文件, 目录)。
fn find_clang_rt_osx() -> Option<(PathBuf, PathBuf)> {
    let out = Command::new("xcode-select").arg("-p").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let devdir = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let mut candidates = vec![];
    // clang 运行时位于 Toolchains/<TC>/usr/lib/clang/...（也兼容旧布局 usr/lib/clang）
    if let Ok(rd) = std::fs::read_dir(PathBuf::from(&devdir).join("Toolchains")) {
        for e in rd.flatten() {
            collect_a_files(&e.path().join("usr/lib/clang"), &mut candidates);
        }
    }
    collect_a_files(&PathBuf::from(&devdir).join("usr/lib/clang"), &mut candidates);
    for p in candidates {
        let is_it = p.to_string_lossy().contains("darwin")
            && p.file_name().and_then(|s| s.to_str()) == Some("libclang_rt.osx.a");
        if is_it {
            let dir = p.parent()?.to_path_buf();
            return Some((p, dir));
        }
    }
    None
}

fn collect_a_files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                collect_a_files(&p, out);
            } else if p.extension().and_then(|s| s.to_str()) == Some("a") {
                out.push(p);
            }
        }
    }
}


fn swiftc_available() -> bool {
    Command::new("xcrun")
        .args(["--find", "swift"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run(cmd: &mut Command) -> bool {
    match cmd.status() {
        Ok(st) => st.success(),
        Err(e) => {
            println!("cargo:warning=无法执行 {:?}: {e}", cmd.get_program());
            false
        }
    }
}

