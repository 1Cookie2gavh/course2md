#!/usr/bin/env bash
# macOS Apple Silicon 基准：速度 + 功耗（powermetrics 需要 sudo 凭据）。
# 用法: packaging/bench-mac.sh <video> [output-dir]
set -uo pipefail

VIDEO="${1:?usage: bench-mac.sh <video> [outdir]}"
OUT="${2:-/tmp/c2m-bench}"
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$REPO/target/release/course2md"
PM_LOG="/tmp/c2m-pm.log"

test -x "$BIN" || { echo "missing $BIN (run cargo build --release first)" >&2; exit 1; }
sudo -v || { echo "sudo required for powermetrics sampling" >&2; exit 1; }

run_one() {
  local label="$1"; shift
  echo "=== $label"
  rm -rf "$OUT"
  sudo powermetrics -s cpu_power,gpu_power,ane_power -i 1000 > "$PM_LOG" 2>/tmp/pm-err.log &
  local pm_pid=$!
  sleep 1
  local t0=$(date +%s)
  local full stats
  full=$("$BIN" "$VIDEO" -o "$OUT" --no-llm "$@" 2>&1 || true)
  local t1=$(date +%s)
  stats=$(printf '%s\n' "$full" | sed 's/\x1b\[[0-9;]*m//g' | grep -E "asr done|Elapsed|Peak memory" | tail -3 || true)
  sudo pkill -f powermetrics || true
  wait $pm_pid 2>/dev/null || true
  local dur=$((t1 - t0))
  local cpu gpu ane n
  n=$(grep -c "CPU Power" "$PM_LOG" || true)
  cpu=$(awk '/^CPU Power/ {s+=$3; n++} END {if(n) printf "%.2f", s/n}' "$PM_LOG")
  gpu=$(awk '/^GPU Power/ {s+=$3; n++} END {if(n) printf "%.2f", s/n}' "$PM_LOG")
  ane=$(awk '/^ANE Power/ {s+=$3; n++} END {if(n) printf "%.2f", s/n}' "$PM_LOG")
  # 能量 = 平均功率 × 时长
  local j cpuj gpuj anej
  cpuj=$(awk -v p="${cpu:-0}" -v d="$dur" 'BEGIN {printf "%.0f", p*d/1000}')
  gpuj=$(awk -v p="${gpu:-0}" -v d="$dur" 'BEGIN {printf "%.0f", p*d/1000}')
  anej=$(awk -v p="${ane:-0}" -v d="$dur" 'BEGIN {printf "%.0f", p*d/1000}')
  echo "wall=${dur}s avg_cpu=${cpu:-0}mW avg_gpu=${gpu:-0}mW avg_ane=${ane:-0}mW E_cpu=${cpuj}J E_gpu=${gpuj}J E_ane=${anej}J samples=${n:-0}"
  printf '%s\n' "$stats" | sed 's/^/    /'
}

run_one "coreml-qwen3"   --provider coreml --asr-model qwen3
run_one "coreml-whisper" --provider coreml --asr-model whisper
run_one "gpu-llama"      --provider gpu
run_one "cpu-llama"      --provider cpu
echo "=== done"
