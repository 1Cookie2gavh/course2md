//! Bilibili 扫码登录与登录态（Netscape cookie）管理。
//!
//! - `login`：passport 二维码生成 → 手机扫码 → 轮询 → 捕获 SESSDATA/bili_jct 等写盘
//! - 登录态写入 `<配置目录>/auth/bilibili.cookies.txt`（供字幕抓取 / 其它逻辑读取）
//! - 同时尽力同步到 exe 同层 `tools\tmp\bilibili_cookies_manual.txt`
//!   （yt-dlp shim 的手工 cookie 槽，Web/CLI 下载自动带上登录态）
//! - `logout`：清除两处登录态

use crate::config;
use anyhow::{Context, Result, bail};
use std::io::Write;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0 Safari/537.36";

/// 需要保存的 B 站登录 cookie（白名单；其余 set-cookie 忽略）
const COOKIE_NAMES: &[&str] = &[
    "SESSDATA",
    "bili_jct",
    "DedeUserID",
    "DedeUserID__ckMd5",
    "buvid3",
    "buvid4",
];

const GENERATE_URL: &str = "https://passport.bilibili.com/x/passport-login/web/qrcode/generate";
const POLL_URL: &str = "https://passport.bilibili.com/x/passport-login/web/qrcode/poll";
const GENERATE_REF: &str = "https://passport.bilibili.com/";

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(20))
        .build()
}

fn get_json(url: &str, query: &[(&str, &str)], cookie: Option<&str>) -> Result<serde_json::Value> {
    let mut req = agent()
        .get(url)
        .query("source", "main-fe-header")
        .set("User-Agent", UA)
        .set("Referer", GENERATE_REF);
    for (k, v) in query {
        req = req.query(k, v);
    }
    if let Some(c) = cookie {
        req = req.set("Cookie", c);
    }
    let body = req
        .call()
        .with_context(|| format!("请求失败: {url}"))?
        .into_string()
        .with_context(|| "读取响应失败")?;
    serde_json::from_str(&body).with_context(|| "响应不是合法 JSON")
}

/// 登录态 cookie 文件路径（Netscape 格式）。
pub fn cookie_path() -> std::path::PathBuf {
    config::config_dir().join("auth").join("bilibili.cookies.txt")
}

/// yt-dlp shim 的手工 cookie 槽（exe 同层 tools\tmp\...）；找不到也无妨。
fn manual_slot() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let tools = exe.parent()?.parent()?; // bin\course2md.exe -> tools
    Some(tools.join("tmp").join("bilibili_cookies_manual.txt"))
}

/// 读取登录态并拼成 Cookie 请求头（白名单过滤）；无登录态返回 None。
pub fn load_cookie_header() -> Option<String> {
    let s = std::fs::read_to_string(cookie_path()).ok()?;
    let mut vals: Vec<String> = Vec::new();
    for line in s.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() >= 7 && COOKIE_NAMES.contains(&f[5]) {
            vals.push(format!("{}={}", f[5], f[6]));
        }
    }
    if vals.is_empty() {
        None
    } else {
        Some(vals.join("; "))
    }
}

fn persist(entries: &[(String, String)]) -> Result<()> {
    let path = cookie_path();
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)
            .with_context(|| format!("创建配置目录失败: {}", d.display()))?;
    }
    let mut out = String::from("# Netscape HTTP Cookie File\n# course2md Bilibili login; do not share this file.\n");
    for (k, v) in entries {
        out.push_str(&format!(".bilibili.com\tTRUE\t/\tTRUE\t0\t{k}\t{v}\n"));
    }
    let tmp = path.with_extension("cookies.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(out.as_bytes())?;
    }
    std::fs::rename(&tmp, &path).with_context(|| "写入登录态失败")?;
    // 同步到 yt-dlp shim 手工槽（best-effort）
    if let Some(manual) = manual_slot() {
        if let Some(d) = manual.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::copy(&path, &manual);
    }
    Ok(())
}

fn print_qr(url: &str) -> Result<()> {
    use qrcode::render::unicode;
    let code = qrcode::QrCode::new(url.as_bytes()).context("二维码生成失败")?;
    let img = code
        .render::<unicode::Dense1x2>()
        .module_dimensions(1, 1)
        .quiet_zone(false)
        .build();
    println!("{img}");
    Ok(())
}

/// `course2md login-bilibili`：生成二维码并轮询直到扫码成功。
pub fn login() -> Result<()> {
    let j = get_json(GENERATE_URL, &[("url", "https://www.bilibili.com/")], None)?;
    if j["code"].as_i64() != Some(0) {
        bail!("获取二维码失败: {}", j["message"].as_str().unwrap_or("未知错误"));
    }
    let qr_url = j["data"]["url"]
        .as_str()
        .context("响应缺少二维码 URL")?
        .to_string();
    let key = j["data"]["qrcode_key"]
        .as_str()
        .context("响应缺少 qrcode_key")?
        .to_string();

    println!("== Bilibili 登录 ==");
    println!("请用手机 B 站 App 扫描下方二维码（扫一次即可，登录态约 30 天有效）：\n");
    if let Err(e) = print_qr(&qr_url) {
        eprintln!("（终端无法绘制二维码，请手动打开扫码：{qr_url}）\n{e}");
    }
    println!("\n若二维码无法识别，可用浏览器打开: {qr_url}\n等待扫码中…");

    let start = std::time::Instant::now();
    loop {
        std::thread::sleep(std::time::Duration::from_secs(2));
        if start.elapsed() > std::time::Duration::from_secs(180) {
            bail!("等待扫码超时（180s），请重试 course2md login-bilibili");
        }
        let poll = get_json(POLL_URL, &[("qrcode_key", &key)], None)?;
        let st = poll["data"]["code"].as_i64().unwrap_or(-1);
        match st {
            0 => {
                // 成功：从 set-cookie 里收集登录态（响应体也带 url，不代表登录态）
                let resp = agent()
                    .get(POLL_URL)
                    .query("qrcode_key", &key)
                    .query("source", "main-fe-header")
                    .set("User-Agent", UA)
                    .set("Referer", GENERATE_REF)
                    .call()
                    .context("轮询请求失败")?;
                let mut entries: Vec<(String, String)> = Vec::new();
                for c in resp.all("set-cookie") {
                    if let Some((k, v)) = c.split_once('=') {
                        let k = k.trim();
                        let v = v.split(';').next().unwrap_or("").trim().to_string();
                        if COOKIE_NAMES.contains(&k) && !v.is_empty() && !entries.iter().any(|(a, _)| a == k) {
                            entries.push((k.to_string(), v));
                        }
                    }
                }
                if entries.is_empty() {
                    bail!("扫码成功但未捕获到登录 cookie，请重试");
                }
                persist(&entries)?;
                println!("✅ Bilibili 登录成功。预览、字幕和视频下载将自动使用此登录状态。");
                println!("登录态: {}", cookie_path().display());
                if let Some(m) = manual_slot() {
                    println!("已同步到 yt-dlp cookie 槽: {}", m.display());
                }
                println!("到期后重新执行 `course2md login-bilibili` 即可续期。");
                return Ok(());
            }
            86038 => bail!("二维码已失效，请重试 course2md login-bilibili"),
            86090 => println!("已扫码，请在手机上确认登录…"),
            86101 => print!("."),
            other => {
                println!("（状态 {other}，继续等待）");
                std::io::stdout().flush().ok();
            }
        }
    }
}

/// `course2md logout-bilibili`：清除登录态（两处文件）。
pub fn logout() -> Result<()> {
    for p in [cookie_path()].into_iter().chain(manual_slot()) {
        if p.is_file() {
            std::fs::remove_file(&p)
                .with_context(|| format!("删除失败: {}", p.display()))?;
        }
    }
    println!("已清除 Bilibili 登录态。");
    Ok(())
}
