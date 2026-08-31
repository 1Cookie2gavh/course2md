#!/usr/bin/env zsh
set -euo pipefail

# 安装 course2md 到 ~/bin，并写入 ~/.zshrc 的 PATH。
# 缺少 ffmpeg / yt-dlp / llama-server 时直接退出。
# 用法：
#   curl -fsSL https://raw.githubusercontent.com/mizorewww/course2md/main/install.sh | zsh
#   或在仓库根目录：./install.sh

REPO="${COURSE2MD_REPO:-mizorewww/course2md}"
BIN_DIR="${COURSE2MD_BIN_DIR:-$HOME/bin}"
ASSET="course2md-macos-arm64"
ZSHRC="${ZDOTDIR:-$HOME}/.zshrc"

missing=()
need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    missing+=("$1")
  fi
}
need ffmpeg
need ffprobe
need yt-dlp
need llama-server

if (( ${#missing[@]} > 0 )); then
  echo "缺少依赖：${(j:, :)missing}" >&2
  echo "请先安装：brew install ffmpeg yt-dlp llama.cpp" >&2
  exit 1
fi

mkdir -p "$BIN_DIR"
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT

if command -v gh >/dev/null 2>&1; then
  gh release download -R "$REPO" -p "$ASSET" -O "$tmp" --clobber
else
  url="$(ASSET="$ASSET" curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" | python3 -c 'import json,os,sys; n=os.environ["ASSET"]; d=json.load(sys.stdin); print(next(a["browser_download_url"] for a in d["assets"] if a["name"]==n))')"
  curl -fsSL "$url" -o "$tmp"
fi

install -m 755 "$tmp" "$BIN_DIR/course2md"

path_line='export PATH="$HOME/bin:$PATH"'
if [[ ! -f "$ZSHRC" ]] || ! grep -Fqs '$HOME/bin' "$ZSHRC"; then
  printf '\n# course2md\n%s\n' "$path_line" >> "$ZSHRC"
  echo "已写入 ${ZSHRC}：${path_line}"
fi

echo "已安装：${BIN_DIR}/course2md"
echo "当前终端执行：export PATH=\"\$HOME/bin:\$PATH\""
echo "首次运行会自动下载识别模型，期间请不要退出。"
