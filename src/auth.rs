//! Bilibili 扫码登录与登录态（Netscape cookie）管理。
//!
//! - `login`：passport 二维码生成 → 手机扫码 → 轮询；**登录 cookie 不在 poll 的
//!   JSON 里**，而在「回调 URL（query 里可能内嵌 cookie）+ crossDomain 票据经多次
//!   302 跳转、每一跳 set-cookie」的链路上，因此用 `redirects(0)` 的 agent 逐跳
//!   手动跟随并累积 set-cookie（与官方 v2 同款算法）。
//! - 登录态写入 `<配置目录>/auth/bilibili.cookies.txt`（供字幕抓取读取），并尽力
//!   同步到 exe 同层 `tools\tmp\bilibili_cookies_manual.txt`（yt-dlp shim 手工槽）。
//! - `logout`：清除两处登录态。

use crate::config;
use anyhow::{Context, Result, bail};
use std::collections::BTreeMap;
use std::io::Write;
use std::time::Duration;

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/152.0 Safari/537.36";
const PASSPORT: &str = "https://passport.bilibili.com/x/passport-login/web/qrcode";

/// 需要保存的 B 站登录 cookie 白名单（其余 set-cookie/query 忽略）。
const COOKIE_NAMES: &[&str] = &[
    "SESSDATA",
    "bili_jct",
    "DedeUserID",
    "DedeUserID__ckMd5",
    "buvid3",
    "buvid4",
];

type Cookies = BTreeMap<String, String>;

fn insert_cookie(jar: &mut Cookies, name: &str, value: &str) {
    let value = value.trim();
    if COOKIE_NAMES.contains(&name)
        && !value.is_empty()
        && !value.chars().any(|c| c.is_control() || c == ';')
    {
        jar.insert(name.to_string(), value.to_string());
    }
}

fn read_cookies(resp: &ureq::Response, jar: &mut Cookies) {
    for header in resp.all("set-cookie") {
        if let Some((name, value)) = header
            .split(';')
            .next()
            .and_then(|pair| pair.split_once('='))
        {
            insert_cookie(jar, name.trim(), value.trim());
        }
    }
}

fn complete(jar: &Cookies) -> bool {
    ["SESSDATA", "bili_jct", "DedeUserID"]
        .iter()
        .all(|k| jar.contains_key(*k))
}

fn jar_header(jar: &Cookies) -> String {
    jar.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

fn login_agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(12))
        .redirects(0)
        .user_agent(UA)
        .build()
}

fn req(agent: &ureq::Agent, url: &str, jar: &Cookies) -> Result<ureq::Response> {
    agent
        .get(url)
        .set("Referer", "https://passport.bilibili.com/")
        .set("Cookie", &jar_header(jar))
        .call()
        .map_err(|e| anyhow::anyhow!("Bilibili 登录请求失败（{url}）：{e}"))
}

fn json_data(resp: ureq::Response) -> Result<serde_json::Value> {
    let j: serde_json::Value = resp
        .into_string()
        .context("Bilibili 登录响应读取失败")?
        .parse()
        .context("Bilibili 登录响应非 JSON")?;
    if j["code"].as_i64() != Some(0) {
        bail!("Bilibili 登录接口返回错误: {}", j["message"].as_str().unwrap_or(""));
    }
    Ok(j["data"].clone())
}

/// 只允许 https://*.bilibili.com 的跳转目标（防票据被带往第三方）。
fn trusted_ticket_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim().trim_start_matches('"');
    if let Some(rest) = trimmed.strip_prefix("https://") {
        let host = rest.split(['/', '?', '#']).next().unwrap_or("");
        let is_bili = host == "bilibili.com" || host.ends_with(".bilibili.com");
        if is_bili && !host.contains('@') {
            return Ok(trimmed.to_string());
        }
    }
    bail!("Bilibili 登录跳转地址不受支持: {raw}")
}

/// 登录成功后补全凭据：回调 query 内嵌 cookie（旧版接口）+ crossDomain 票据链跟随。
fn finish_login(agent: &ureq::Agent, login_data: &serde_json::Value, jar: &mut Cookies) -> Result<()> {
    if let Some(raw) = login_data["url"].as_str().filter(|s| !s.is_empty()) {
        // 1) 旧版接口：回调 query 里直接带凭据
        if let Some(q) = raw.split_once('?').map(|(_, q)| q) {
            for pair in q.split('&') {
                if let Some((name, value)) = pair.split_once('=')
                    && !jar.contains_key(name)
                {
                    insert_cookie(jar, name, value);
                }
            }
        }
        // 2) 新版：crossDomain 票据 → 逐跳 302，每跳累积 set-cookie
        if !complete(jar) {
            let mut next = trusted_ticket_url(raw)?;
            for _ in 0..6 {
                let resp = req(agent, &next, jar)?;
                read_cookies(&resp, jar);
                if complete(jar) {
                    break;
                }
                if !(300..400).contains(&resp.status()) {
                    break;
                }
                let location = resp
                    .header("location")
                    .context("登录跳转缺少 Location")?;
                let joined = if location.starts_with("http://") || location.starts_with("https://") {
                    location.to_string()
                } else if let Some(prefix) = next.split_once('/').map(|(s, _)| s) {
                    // 相对跳转：拼回原主机
                    format!("{prefix}/{location}")
                } else {
                    bail!("登录跳转地址无效: {location}");
                };
                next = trusted_ticket_url(&joined)?;
            }
        }
    }
    if !complete(jar) {
        bail!(
            "登录响应缺少完整凭据（已捕获: {}）。请重试 course2md login-bilibili",
            jar.keys().cloned().collect::<Vec<_>>().join(", ")
        );
    }
    Ok(())
}

fn persist(entries: &Cookies) -> Result<()> {
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
    let agent = login_agent();
    let mut jar = Cookies::new();

    // 1) 生成二维码
    let resp = req(
        &agent,
        &format!("{PASSPORT}/generate?source=main-fe-header&url=https://www.bilibili.com/"),
        &jar,
    )?;
    read_cookies(&resp, &mut jar);
    let qr = json_data(resp)?;
    let link = qr["url"].as_str().context("响应缺少二维码 URL")?.to_string();
    let key = qr["qrcode_key"].as_str().context("响应缺少 qrcode_key")?.to_string();

    println!("== Bilibili 登录 ==");
    println!("请用手机 B 站 App 扫描下方二维码（登录态约 30 天有效）：\n");
    if let Err(e) = print_qr(&link) {
        eprintln!("（终端无法绘制二维码，请手动打开扫码：{link}）\n{e}");
    }
    println!("\n若二维码无法识别，可用浏览器打开: {link}\n等待扫码中…");

    // 2) 轮询
    let start = std::time::Instant::now();
    loop {
        if start.elapsed() > Duration::from_secs(180) {
            bail!("等待扫码超时（180s），请重试 course2md login-bilibili");
        }
        std::thread::sleep(Duration::from_secs(2));
        let poll_url = format!("{PASSPORT}/poll?qrcode_key={key}&source=main-fe-header");
        let resp = match req(&agent, &poll_url, &jar) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("轮询请求失败，稍后重试：{e}");
                continue;
            }
        };
        read_cookies(&resp, &mut jar); // 会话/票据 cookie 累积
        let data = match json_data(resp) {
            Ok(d) => d,
            Err(_) => continue,
        };
        match data["code"].as_i64().unwrap_or(-1) {
            0 => {
                // 3) 登录成功：补全凭据（回调 query / crossDomain 票据链）
                finish_login(&agent, &data, &mut jar)?;
                persist(&jar)?;
                println!();
                println!("✅ Bilibili 登录成功。预览、字幕和视频下载将自动使用此登录状态。");
                println!("登录态: {}", cookie_path().display());
                if let Some(m) = manual_slot() {
                    println!("已同步到 yt-dlp cookie 槽: {}", m.display());
                }
                println!("到期后重新执行 `course2md login-bilibili` 即可续期。");
                return Ok(());
            }
            86038 => bail!("二维码已失效，请重试 course2md login-bilibili"),
            86090 => {
                println!();
                println!("已扫码，请在手机上确认登录…");
            }
            86101 => print!("."),
            other => {
                eprintln!("\n（未知状态 {other}，继续等待）");
            }
        }
        std::io::stdout().flush().ok();
    }
}

/// `course2md logout-bilibili`：清除登录态（两处文件）。
pub fn logout() -> Result<()> {
    for p in [cookie_path()].into_iter().chain(manual_slot()) {
        if p.is_file() {
            std::fs::remove_file(&p).with_context(|| format!("删除失败: {}", p.display()))?;
        }
    }
    println!("已清除 Bilibili 登录态。");
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookies_sanitized_and_whitelisted() {
        let mut j = Cookies::new();
        // 合法 cookie
        let resp: ureq::Response = "HTTP/1.1 200 OK\r\nSet-Cookie: SESSDATA=abc%2C123; Domain=.bilibili.com; HttpOnly; Secure\r\nSet-Cookie: bili_jct=csrf; Path=/\r\nSet-Cookie: DedeUserID=123; Path=/\r\n\r\n".parse().unwrap();
        read_cookies(&resp, &mut j);
        assert!(complete(&j), "三个核心 cookie 应齐备");
        assert_eq!(j["SESSDATA"], "abc%2C123");
        // 非白名单 cookie 被忽略；白名单 cookie 可正常覆盖
        let more: ureq::Response = "HTTP/1.1 200 OK\r\nSet-Cookie: evil=1; Path=/\r\nSet-Cookie: bili_jct=newcsrf; Path=/\r\n\r\n".parse().unwrap();
        read_cookies(&more, &mut j);
        assert!(!j.contains_key("evil"), "白名单之外的 cookie 不落盘");
        assert_eq!(j["bili_jct"], "newcsrf");
    }

    #[test]
    fn callback_query_embeds_credentials() {
        // 旧版接口：凭据在回调 query 里
        let data = serde_json::json!({
            "url": "https://passport.bilibili.com/login?code=0&SESSDATA=q%2C1&bili_jct=tk&DedeUserID=7"
        });
        let agent = login_agent();
        let mut j = Cookies::new();
        finish_login(&agent, &data, &mut j).unwrap();
        assert!(complete(&j));
        assert_eq!(j["SESSDATA"], "q%2C1");
    }

    #[test]
    fn trusted_ticket_rejects_foreign_host() {
        assert!(trusted_ticket_url("https://www.bilibili.com/").is_ok());
        assert!(trusted_ticket_url("https://evil.com/steal").is_err());
        assert!(trusted_ticket_url("http://www.bilibili.com/").is_err());
        assert!(trusted_ticket_url("https://user@www.bilibili.com/").is_err());
    }
}
