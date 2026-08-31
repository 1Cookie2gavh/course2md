# 验收记录

## 目标

使用完整管线处理指定课程视频并产出可读的图文课程笔记：

```
course2md run "https://www.bilibili.com/video/BV1pb8o6yE8f"
```

- 视频：《欢迎来到未来 [01-Raw/26生成式软件工程/NJU]》，UP 主「绿导师原谅你了」
- 时长 5999s（≈100 分钟），1280×410 宽幅课件录屏

## 环境

- macOS arm64（Apple Silicon）
- ffmpeg 9.0.1、yt-dlp（brew）
- 模型：Qwen3-ASR **1.7B int8**（ModelScope `zengshuishui/Qwen3-ASR-onnx`）+ Silero VAD
  （GitHub k2-fsa asr-models release）
- ASR 线程数：6

## 结果

（管线运行后填写）

| 指标 | 值 |
|---|---|
| 场景候选（阈值 0.35） | 33 |
| 冷却 10s 后 | 23 |
| dHash 去重后保留 | 22 帧 |
| VAD 语音段 | 707 段 / 5261s（占 87.7%）|
| ASR RTF | ~0.28 |
| 总耗时 | 待填 |

## 人工抽查

- [ ] course.md 打开可读，截图清晰、与文字时段对应
- [ ] course.html 图片懒加载、时间戳链接跳转正确（`?t=`）
- [ ] structured.json 可被 `jq` 解析
- [ ] 抽查 3 处时间戳：转写文本与视频中语音内容一致

## 复现

```bash
course2md models download --size 1.7b
course2md run "https://www.bilibili.com/video/BV1pb8o6yE8f" -o out-bv
```
