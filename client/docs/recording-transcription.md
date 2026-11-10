# 录音转写工作台

录音中增量显示转写原文。结束录音后先保存音频，再补齐未处理的录音。中断或失败的历史录音可以从已保存的音频继续。旧录音的转写和人工校对会保留。

## 本地转写和会议纪要

- X-ASR 中文和英文 INT8 模型输出文字、标点和时间戳。录音统一写入 16 kHz 单声道 PCM，增量转写优先从静音处切分，连续语音窗口约 4 秒。首段耗时包括推理时间，打开录音工作台时会在后台预加载模型。
- pyannote segmentation 3.0 标记发言区间，ERes2Net-Base 提取声纹特征。同一录音内用相邻窗口的重叠区间和声纹特征延续说话人编号。说话人标签可以校对，重叠发言、短促插话和噪声可能导致错分。没有足够依据的片段显示「说话人未确定」。
- 模型和推理程序随安装包交付，运行时直接读取本机资源。推理失败时保留音频和已落盘的文字，支持从游标继续。暂停不计入录音时间轴。
- 录音默认只进行本地转写。可在开始录音前为本次录音开启自动纪要，或在详情页手动生成。页面会显示生效的默认聊天模型和接收地址，生成时向该连接发送会议标题与转写原文。默认模型或连接变更后，需要重新确认接收方。模型不可用或输出不完整时，已有纪要、原文和音频仍可使用。

## 回听和校对

- 点击纪要中的有效原文时间依据只定位。点击原文时间戳从该位置播放。
- 内置播放器支持进度、倍速、前后十秒及音量，播放失败时可使用系统播放器。
- 音频通过仅授权会议 ID 与临时令牌的私有协议读取。单次范围读取不超过 2 MiB。
- 校对草稿自动保留，显式保存后更新正式内容。可以修改单段说话人，也可以将新名字应用到所有原来同名的段落。遇到冲突时会提示合并。修改原文后保留已有纪要并提示核对，重新生成时保留上一次纪要供恢复。
- 每条录音最多维护 64 个声纹轨迹。无法继续匹配编号的段落仍会转写，用户可以手动校对。会议文字达到本机存储上限时，保留上次保存的文字，录音仍可继续保存和回听。

## 构建和验证

Tauri 开发和打包前运行 `pnpm meeting:speech:prepare`。本地缓存缺少所需文件时，从 [meeting-speech-models-v1 Release](https://github.com/fuyuxiang/echo-agent-desktop/releases/tag/meeting-speech-models-v1) 下载三个模型包和当前目标的 sherpa-onnx 运行库。脚本验证下载文件的大小和 SHA-256，再验证解包后的每个模型文件。

离线构建时，将三个原始模型包和目标平台的运行库放在同一目录，保留附件文件名并提前缓存 Cargo 依赖。运行 `node scripts/prepare-meeting-speech.mjs --offline --source <归档目录>`，可通过 `--target` 指定 Rust 目标。资源准备后用 `pnpm meeting:speech:check` 验证模型。

`pnpm test:ui:meetings` 使用生产组件、真实 WAV 和模拟 IPC，覆盖播放、增量更新、自动纪要、保存重试和草稿冲突。会议单元测试位于 `src/components/__tests__` 和 `src/lib/__tests__`。

原生会议测试可用 `cargo test --manifest-path src-tauri/Cargo.toml --locked --lib meeting_minutes::` 运行。设置 `ECHO_MEETING_TEST_WORKER` 为已构建的推理程序绝对路径，`ECHO_MEETING_TEST_WAV` 为 [公开四人中文样本](https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-segmentation-models/0-four-speakers-zh.wav) 的绝对路径，可执行离线真实推理。样本 SHA-256 为 `bedf036caed208386c67b4ef4b11f83d74dd0d420b102163a1c33cd09cde7010`。
