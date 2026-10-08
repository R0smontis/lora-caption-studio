# LoRA Caption Studio

Windows 桌面端 LoRA 数据集打标、复核与文本整理工具（Tauri 2 + React + Rust）。支持主流云端多模态 API（含多渠道叠加与思考模型）与本地 WD Tagger 离线打标（CUDA / DirectML / CPU 自动回退）。

---
## 快速开始

1. **配置服务**：进入「API 连接」添加服务商，可使用一键预设补全地址并填入 Key；或使用「WD tagger」下载离线模型。
2. **准备图片**：
   - 直接导入：在「打标任务」中选择单张、多张或整文件夹（可勾选包含子文件夹）；
   - 或先抽样：进入「抽选」页，按数量随机抽取并一键隔离其余未选图片至 `.refusal/`。
3. **设置参数**：在任务控制区配置渠道（支持多连接与各渠道模型）、提示词与高级参数（温度、超时、思考额度等）。
4. **启动打标/复核**：
   - 点击「开始打标」，标签自动写入同名 `.txt`；
   - 或切换至「模型复核」，对照已有标签进行针对性修正；支持随时中止，后续重开自动跳过已处理项目。
5. **精细整理**：使用「批量编辑」增删修改标签，或在「版本历史」中随时对比差异并按批次批量恢复历史状态。

---

## 核心功能模块

### 1. 图像打标与模型复核
- **双模态工作流**：支持「生成标签」（从零打标）与「模型复核」（对照图片修订已有 `.txt` 标签）。
- **图片尺寸自适应控制**：提交 API 前自动检测分辨率，长边超过 2048px (2K) 自动等比缩放为高质量 JPEG，未超限原样发送，避免超时与服务端体积限制。
- **复核安全与格式保护**：
  - **截断与骤减防护**：复核输出比原内容减少超过 50% 时自动拦截并不覆盖原文件，标记错误（支持界面一键手动忽略）。
  - **标签格式自动规范**：自动统一标签分隔格式为 `tag1, tag2, tag3`（逗号加单空格），清理多余空格与空段。
  - **复核去重与修订计数**：已复核成功的图片自动标记，停止任务后重开默认跳过；直观展示各图片累计被修订次数（`修订×N`）。
- **图片检索与排序**：支持按文件名、标签内容或错误信息搜索，支持按文件名自然序、修订次数、标签字数升降序排列。

### 2. 多模型接入与多渠道叠加
- **广泛的 API 协议兼容**：
  - OpenAI 兼容格式、Anthropic 原生接口、Google Gemini 原生多模态接口。
  - 内置 16 家常用服务商预设（火山方舟/豆包、OpenAI、DeepSeek、SiliconFlow、Kimi、通义、混元、Ollama、LM Studio 等）。
  - 自动识别并剥离深度思考模型内容（如 `<think>...</think>`），避免推理过程污染数据集标签。
- **多渠道叠加 (Multi-Channel Routing)**：
  - 支持配置多组独立的「连接 + 模型」组合（可多开也可单选）。
  - 各渠道 Key 槽位统一池化轮询分发，单渠道配额耗尽或受限自动故障隔离。
- **弹性并发与重试**：
  - 每 Key 独立并发限制，最少活动 Key 优先；
  - 429 自动读取 `Retry-After` 退避；网络瞬态异常（503/超时/连接断开）自动重试最多 6 次。
  - 最终无响应项目标记为「无响应」，不污染成功/错误统计与性能指标。

### 3. 本地离线打标 (WD Tagger)
- **内置模型家族**：支持 WD v1.4 (ViT, ConvNeXt, SwinV2, Moat) 与 v3 / EVA02 等 7 款经典打标模型，按需下载。
- **硬件加速引擎 (ONNX Runtime)**：
  - 优先启用 **NVIDIA CUDA EP**（支持 TensorFloat-32 加速）；
  - 失败时自动降级尝试 **DirectML EP**（DX12 通用 GPU 加速）；
  - 兜底回退 **CPU 离线推理**，并在任务日志中实时汇报实际运行设备。
- **打标参数调校**：支持置信度阈值（Threshold）、最多标签数限制（Max Tags）以及下划线转换开关。

### 4. 智能抽选与数据集管理
- **随机抽选**：从文件夹随机抽取指定数量图片打标，未选中图片可一键移入 `.refusal/` 归档。
- **AI 智能抽样**：
  - 支持多轮迭代筛选（按用户自然语言要求过滤）；
  - 结果实时流转为「AI 通过 / 待分析 / AI 丢弃」，支持多轮评分与人工干预微调。
- **清单可视化预览**：支持在抽样/打标前双列表对比保留项与丢弃项，支持跨区拖动、批量移动与关键词搜索。

### 5. 批量编辑与版本历史
- **规则化批量修改**：
  - Tag 模式与纯文本模式，支持插入（开头/结尾/指定位置/锚点前后）、替换与正则删除。
  - **缺失检测添加**：支持「仅当内容缺失时添加」，已包含指定 Tag 时自动跳过，避免重复追加。
  - 应用前实时 Diff 高亮预览，支持冲突检测。
- **全链路版本恢复**：
  - 任何标签覆盖均记录 SQLite 版本变更历史，支持单文件任意版本恢复或整批撤销；
  - 版本历史支持多选批量恢复，自动去重同一文件的多次历史只恢复最新项。

### 6. 数据集辅助工具
- **Tag 词频统计**：实时统计当前数据集高频词覆盖率与出现频次。
- **Anima LoRA 训练参数设计器**：依据图片数量、LoRA 类型与显卡 Batch Size，根据模型规范自动测算最佳 Epoch、Repeat、Steps 并生成数据集 TOML 配置。
- **Safetensors 模型后处理**：批量清除 LoRA 模型的 `__metadata__` 私有训练参数，原文件保留并经 SHA-256 完整性校验。

---

## 数据安全与容灾

应用数据保存在当前用户应用数据目录中（`%APPDATA%\local.lora.captionstudio`），**完全独立于程序安装目录**：

| 数据项 | 存储路径 / 机制 | 安全与备份策略 |
|---|---|---|
| **配置与元数据** | `caption-studio.db` (SQLite WAL) | 每日首次启动自动在线快照；配置每次更新自动备份最近 20 份快照 |
| **敏感凭据** | `secrets.bin` | Windows DPAPI 用户级加密存储，内存按需解密，日志全脱敏 |
| **本地模型** | `models\` | 独立目录存储，重装与版本升级均原样保留 |
| **灾难恢复** | 损坏自动隔离 | 若 DB 或凭据文件被外部损坏，自动隔离为 `.corrupt` 并以干净状态启动，杜绝白屏崩溃 |

> **提示**：更新或重新安装客户端**不会丢失任何模型和配置**。

---

## 开发与构建

### 环境要求
- Node.js >= 18 + npm
- Rust >= 1.80 + Cargo
- Windows 10/11 x64

### 本地开发
```powershell
npm.cmd install
npm.cmd test
cargo test --manifest-path src-tauri\Cargo.toml
npm.cmd run tauri dev
```

### 自动化验证套件
```powershell
cargo fmt --manifest-path src-tauri\Cargo.toml -- --check
cargo clippy --manifest-path src-tauri\Cargo.toml --all-targets
cargo test --manifest-path src-tauri\Cargo.toml        # 包含端到端模拟与调度测试
npm.cmd test                                            # 前端组件与功能测试
npx tsc --noEmit                                        # 类型系统检查
python scripts\security_scan.py                         # 源码凭据泄露审计
```

### 构建安装包
```powershell
CI=false npm.cmd run tauri build
```
- 输出程序：`src-tauri\target\release\lora-caption-studio.exe`
- 输出安装包：`src-tauri\target\release\bundle\nsis\LoRA Caption Studio_0.1.0_x64-setup.exe`

### 运行时资源（源码编译说明）
仓库排除大体积运行库以保持精简，自行打包前需将以下库放置在 `src-tauri/resources/`：
- **`resources/onnxruntime/`**：`onnxruntime.dll`、`onnxruntime_providers_cuda.dll`、`onnxruntime_providers_shared.dll`（取自 NuGet `Microsoft.ML.OnnxRuntime.Gpu.Windows` 1.24+ 或 DirectML 包）。
- **`resources/cuda/`**：CUDA 12.8 及 cuDNN 9 运行时动态库（`cudart64_12.dll`、`cublas64_12.dll`、`cublasLt64_12.dll`、`cufft64_11.dll` 与 `cudnn*64_9.dll`）。

