#!/usr/bin/env bash
set -euo pipefail

# 安装 course2md 预编译二进制到 ~/bin（或 COURSE2MD_BIN_DIR）。
# 用法：
#   curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | bash

REPO="${COURSE2MD_REPO:-mizorewww/course2md}"
BIN_DIR="${COURSE2MD_BIN_DIR:-$HOME/bin}"

os="$(uname -s | tr '[:upper:]' '[:lower:]')"
arch="$(uname -m)"
case "$os-$arch" in
  darwin-arm64|darwin-aarch64) ASSET="course2md-macos-arm64" ;;
  darwin-x86_64) ASSET="course2md-macos-x86_64" ;;
  linux-x86_64|linux-amd64) ASSET="course2md-linux-x86_64" ;;
  linux-aarch64|linux-arm64) ASSET="course2md-linux-aarch64" ;;
  *)
    echo "暂无预编译包：$os $arch。请用 cargo install --path . 从源码安装。" >&2
    exit 1
    ;;
esac

missing=()
need() { command -v "$1" >/dev/null 2>&1 || missing+=("$1"); }
need ffmpeg
need ffprobe
need yt-dlp
need llama-server
if [ "${#missing[@]}" -gt 0 ]; then
  echo "缺少依赖：${missing[*]}" >&2
  echo "macOS:  brew install ffmpeg yt-dlp llama.cpp" >&2
  echo "Arch:   sudo pacman -S ffmpeg yt-dlp llama-cpp" >&2
  echo "Debian: sudo apt install ffmpeg yt-dlp && 安装 llama.cpp（见 README）" >&2
  exit 1
fi

mkdir -p "$BIN_DIR"
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

if command -v gh >/dev/null 2>&1; then
  gh release download -R "$REPO" -p "$ASSET" -O "$tmp" --clobber
else
  url="$(
    COURSE2MD_REPO="$REPO" ASSET="$ASSET" python3 - <<'PY'
import json, os, urllib.request
repo = os.environ["COURSE2MD_REPO"]
asset = os.environ["ASSET"]
with urllib.request.urlopen(f"https://api.github.com/repos/{repo}/releases/latest") as r:
    data = json.load(r)
for a in data.get("assets", []):
    if a["name"] == asset:
        print(a["browser_download_url"])
        break
else:
    raise SystemExit(f"release 中找不到 {asset}")
PY
  )"
  curl -fsSL "$url" -o "$tmp"
fi

install -m 755 "$tmp" "$BIN_DIR/course2md"
echo "已安装：$BIN_DIR/course2md"
echo "请确保 PATH 包含 $BIN_DIR，例如：export PATH=\"\$HOME/bin:\$PATH\""
echo "首次运行会自动下载识别模型（约 2.4GB），期间请不要退出。"
