#!/usr/bin/env python3
"""Qwen3-ASR 1.7B on Apple Silicon MPS (Metal GPU).

读整段 16k wav + VAD 时间戳 JSONL，批量送 GPU，写出 transcript JSONL。
"""
from __future__ import annotations

import argparse
import json
import sys
import time
from pathlib import Path

import numpy as np
import torch
from qwen_asr import Qwen3ASRModel


def load_wav_16k(path: Path) -> np.ndarray:
    import wave

    with wave.open(str(path), "rb") as w:
        if w.getnchannels() != 1:
            raise SystemExit("need mono wav")
        sr = w.getframerate()
        sw = w.getsampwidth()
        raw = w.readframes(w.getnframes())
    if sw != 2:
        raise SystemExit(f"unsupported sample width {sw}")
    if sr != 16000:
        raise SystemExit(f"need 16k wav, got {sr}")
    return np.frombuffer(raw, dtype=np.int16).astype(np.float32) / 32768.0


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default="Qwen/Qwen3-ASR-1.7B")
    ap.add_argument("--wav", required=True)
    ap.add_argument("--segments", required=True, help="jsonl {start,end}")
    ap.add_argument("--out", required=True)
    ap.add_argument("--batch", type=int, default=8)
    ap.add_argument("--language", default="Chinese")
    args = ap.parse_args()

    if not torch.backends.mps.is_available():
        print("ERROR: MPS 不可用，无法使用 GPU", file=sys.stderr)
        return 2

    segs = []
    with open(args.segments) as f:
        for line in f:
            line = line.strip()
            if line:
                segs.append(json.loads(line))
    if not segs:
        Path(args.out).write_text("")
        return 0

    print(f"[mps] loading {args.model} on mps, {len(segs)} segments, batch={args.batch}", flush=True)
    t0 = time.time()
    model = Qwen3ASRModel.from_pretrained(
        args.model,
        dtype=torch.bfloat16,
        device_map="mps",
        max_inference_batch_size=args.batch,
        max_new_tokens=256,
    )
    print(f"[mps] model ready in {time.time()-t0:.1f}s", flush=True)

    audio = load_wav_16k(Path(args.wav))
    sr = 16000
    out_f = open(args.out, "w")
    done = 0
    t_dec = time.time()
    speech = 0.0
    for i in range(0, len(segs), args.batch):
        batch = segs[i : i + args.batch]
        clips = []
        for s in batch:
            a = int(float(s["start"]) * sr)
            b = int(float(s["end"]) * sr)
            clip = audio[max(0, a) : max(a + 1, min(len(audio), b))]
            if clip.size == 0:
                clip = np.zeros(1600, dtype=np.float32)
            clips.append((clip, sr))
            speech += (b - a) / sr
        results = model.transcribe(audio=clips, language=args.language)
        for s, r in zip(batch, results):
            text = (getattr(r, "text", None) or str(r) or "").strip()
            out_f.write(
                json.dumps(
                    {"start": float(s["start"]), "end": float(s["end"]), "text": text},
                    ensure_ascii=False,
                )
                + "\n"
            )
        out_f.flush()
        done += len(batch)
        el = time.time() - t_dec
        rtf = el / max(speech, 1e-6)
        print(f"[mps] {done}/{len(segs)}  RTF={rtf:.3f}  GPU=mps", flush=True)
    out_f.close()
    el = time.time() - t_dec
    print(f"[mps] done {len(segs)} segs in {el:.1f}s  RTF={el/max(speech,1e-6):.3f}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
