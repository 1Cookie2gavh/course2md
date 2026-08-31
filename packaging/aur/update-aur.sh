#!/usr/bin/env bash
# 渲染 PKGBUILD.template 并推送到 AUR。
# 用法: update-aur.sh <version>   （version 不带 v 前缀，如 0.4.0）
# 需要已配置可推送 AUR 的 SSH key（Host aur.archlinux.org, User aur）。
set -euo pipefail

VERSION="${1:?usage: update-aur.sh <version>}"
REPO="${COURSE2MD_REPO:-mizorewww/course2md}"
PKG=course2md-bin
HERE="$(cd "$(dirname "$0")" && pwd)"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cd "$WORK"
echo "下载 v${VERSION} 产物并计算校验和..."
curl -fsSLo linux-x86_64 "https://github.com/${REPO}/releases/download/v${VERSION}/course2md-linux-x86_64"
curl -fsSLo linux-aarch64 "https://github.com/${REPO}/releases/download/v${VERSION}/course2md-linux-aarch64"
curl -fsSLo LICENSE "https://raw.githubusercontent.com/${REPO}/v${VERSION}/LICENSE"
SHA_X86_64="$(sha256sum linux-x86_64 | cut -d' ' -f1)"
SHA_AARCH64="$(sha256sum linux-aarch64 | cut -d' ' -f1)"
SHA_LICENSE="$(sha256sum LICENSE | cut -d' ' -f1)"

git clone -q "ssh://aur@aur.archlinux.org/${PKG}.git" aur
sed -e "s/@VERSION@/${VERSION}/g" \
    -e "s/@SHA_X86_64@/${SHA_X86_64}/" \
    -e "s/@SHA_AARCH64@/${SHA_AARCH64}/" \
    -e "s/@SHA_LICENSE@/${SHA_LICENSE}/" \
    "${HERE}/PKGBUILD.template" > aur/PKGBUILD

cd aur
makepkg --printsrcinfo > .SRCINFO
git add PKGBUILD .SRCINFO
git diff --cached --quiet && { echo "AUR 已是 ${VERSION}，无需更新"; exit 0; }
git commit -qm "${PKG} ${VERSION}-1"
git push -q origin master
echo "AUR ${PKG} 已更新到 ${VERSION}"
