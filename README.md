# LoRA Caption Studio

Windows 桌面端 LoRA 数据集打标、复核与文本整理工具（Tauri 2 + React + Rust）。

## 主要能力

- OpenAI 兼容、Anthropic、Gemini 原生多模态接口；三种供应商模型列表（Gemini 分页），仅保留支持 `generateContent` 的模型。
- Base URL 自动补全：内置 16 家知名服务商预设（OpenAI、Anthropic、Gemini、DeepSeek、Kimi、智谱、通义、千帆、混元、火山方舟、SiliconFlow、OpenRouter、Groq、Ollama、LM Studio、vLLM），收在一个下拉栏中，选项仅显示名称；选择即填充名称/供应商/Base URL；保存/测试连接时自动尝试 `/v1`、`/v1beta` 等常见布局并回填有效格式。
- 各类推理模型支持任务级“思考额度”：自动识别 OpenAI 兼容、Claude、Gemini 3、Gemini 2.5、Qwen/QwQ 与 DeepSeek，提供跟随默认、关闭/最小、低、中、高、极高和自定义 Token；启动时把映射后的 `reasoning_effort`、`thinking.budget_tokens`、`thinkingLevel`、`thinkingBudget` 或 `enable_thinking/thinking_budget` 冻结进任务快照，不改变连接库的默认参数。
- 本地打标模型（可选）：内置 WD Tagger 系列与更新的 v3/EVA02 系列共 7 个模型（如 wd-v1-4-vit/moat/swinv2、wd-eva02-large-tagger-v3），模型不随应用分发，可手动下载或首次调用自动下载（断点续传 + 进度事件），使用 ONNX Runtime 在本机 CPU 离线推理（NCHW/NHWC 自动适配），不消耗 API 额度；onnxruntime.dll 随安装包分发。模型、断点文件及 CUDA/Hugging Face 缓存默认保存在“文档”所在的非系统盘，若文档位于系统盘则自动选择 D:–Z: 中首个可写本地磁盘；不会下载到 C 盘。可通过 `LORA_CAPTION_MODEL_DIR` 指定其他非系统盘目录。
- 全局 API 连接库：连接测试、可搜索的连接菜单、可搜索/点击/手输的模型组合框、刷新缓存、切换连接自动恢复该连接最近模型。
- 多 Key 并发调度：每 Key 并发限制、最少活动数 + 轮询决胜、429 `Retry-After` 与 5/10 秒回退、认证/额度失败自动停用 Key、最多两次可重试错误迁移、任务快照在启动时冻结。
- 流式解析覆盖 SSE、NDJSON 与 Gemini JSON 数组分块；结构化拒绝、usage、首字延迟与 Token 速度。
- 停止任务立即取消未开始项目并终止网络流，所有队列项目收到明确 `cancelled` 事件；`key-runtime-event` 展示每 Key 活动数、成功、失败、Token、冷却与停用原因。
- 单张、批量、文件夹与可选递归图片导入；全部项目异步懒加载缩略图；打标列表和抽样页均可点击缩略图打开画布式高分辨率查看器，默认完整适配，支持拖拽平移、滚轮/双指连续无级缩放、双击缩放、左右旋转、100% 实际大小、快捷键与一键复位。
- 图片抽选支持随机模式与 AI 模式：AI 结果明确分为“AI 通过 / 待分析 / AI 丢弃”，失败、模型拒绝、超时、取消与无效判定只进入“待分析”。模型给出明确判断后，图片与同名 `.txt` 立即按原相对结构移入 `.ai-selected` 或 `.ai-rejected`；通过和丢弃项均可逐张/批量退回待分析区，每次点击开始只把待分析区加入队列。暂停后继续也只提交尚未分析的图片，重置可把两个结果目录完整恢复到原位置。筛选要求可保存、更新、载入和删除为本地提示词预设；可设置目标保留数量与最多轮数，每轮逐级提高严格程度，最后依据模型 0–100 评分精确收敛。AI 抽样使用每 Key 并发限制。
- 中英文文本拒绝检测、供应商结构化拒绝检测和自定义规则；输入/输出 Token、Token 速度、首字延迟。
- 打标与复核预设均可配置独立“高级模型”（API 提供商连接、模型、思考额度）；候选标签少于原标签一半时自动把原图、原标签和异常候选交由高级模型重新审核，只在返回完整合格标签后覆盖原文。文本拒绝、结构化安全拒绝及服务端提示词拒绝会自动重试两次。
- 打标页内置可搜索的 Tag 频率图：统计每个 Tag 覆盖的图片数、覆盖率与实际出现次数，并随标签写入和手动编辑实时更新。
- Anima LoRA 训练参数设计器：按图片数量、角色/画风/概念/姿态类型、用户指定 Batch Size 与训练强度，自动给出 Epoch、Repeat、预计 Steps、单图呈现次数和可复制的 Anima 数据集 TOML；内置小样本、过大 Batch 与过拟合风险提示，并在界面中列出官方模型卡、训练工具文档和社区实测依据。
- Anima 参数设计器会持续记忆图片数量、LoRA 类型、Batch Size 和训练强度；抽选页面会持续记忆文件夹、模式、筛选要求、API/模型、并发、循环参数以及通过/待分析/丢弃结果，用户主动重置后才清除该次操作记录。
- LoRA 模型后处理：批量移除 `.safetensors` 的整个 `__metadata__` 区块，清除 sd-scripts、ModelSpec 和自定义训练参数；原文件不覆盖，输出后二次扫描并用 SHA-256 验证张量数据逐字节一致。
- 打标与复核预设：新建、复制、重命名、保存、删除（引用检查）；预设级超时与并发参数。
- 版本化 SQLite 迁移：`task_runs`、`task_items`、`edit_batches`、`file_mutations`；指标保存预设 ID、图片路径、重试数与最终状态。
- API Key 与敏感自定义请求头只进入 DPAPI 用户级凭据库；数据库与前端仅含 ID、名称、掩码；日志遮蔽认证头、Key、Bearer 与 URL 敏感查询参数。
- 所有标签覆盖走文件变更日志：登记 → 原子写入 → 提交版本 → 标记完成；崩溃后启动时自动恢复，杜绝虚假版本记录。
- 批量编辑五种范围（当前选中、当前筛选、任务全部、独立 TXT 多选、TXT 文件夹）；预览返回批次 ID，应用时校验文件未被外部修改，冲突文件列出并保留；支持整批撤销与单文件任意版本恢复。
- Tag/纯文本双模式：首尾、索引、行号、锚点、首次/全部匹配、精确/正则删除替换、大小写、多行与稳定去重。
- 全局版本历史从 SQLite 加载（不依赖当前图片列表），按批次组织撤销；统计支持按任务类型、批次、API、模型、预设、Key 分组。
- 日志面板：拖动调整高度、级别与来源双筛选、自动滚动开关、复制、清空；后端五个、每个最多 2 MB 的轮转日志。

## 开发

```powershell
npm.cmd install
npm.cmd test
cargo test --manifest-path src-tauri\Cargo.toml
npm.cmd run tauri dev
```

若终端环境变量 `CI=1` 导致 tauri CLI 报 `invalid value '1' for '--ci'`，构建时使用 `CI=false npm.cmd run tauri build`。

## 验证

```powershell
cargo fmt --manifest-path src-tauri\Cargo.toml -- --check
cargo clippy --manifest-path src-tauri\Cargo.toml --all-targets
cargo test --manifest-path src-tauri\Cargo.toml        # 含本地模拟 HTTP 服务的端到端与调度器测试
npm.cmd test                                            # React 组件测试
npx tsc --noEmit
python scripts\security_scan.py                         # 源码与产物无明文凭据扫描
```

## 构建安装包

```powershell
CI=false npm.cmd run tauri build
```

生成位置：

- 程序：`src-tauri\target\release\lora-caption-studio.exe`
- 安装包：`src-tauri\target\release\bundle\nsis\LoRA Caption Studio_0.1.0_x64-setup.exe`

验收记录（0.1.0）：

- 安装包大小：975,309,934 字节（含 ONNX Runtime 与 CUDA 本地推理运行库）
- 安装包 SHA-256：`FF999955A87B2DB8695C60AC4148AF0146F5CB1C9FB2027AC8CB3D3713647EFD`
- Release 程序 SHA-256：`4175F355C9FEC623A8E1F1BCE1234F6595887EB8EBA19F16734F5E9594AB1C17`
- 已在本机通过：安装、启动、数据库迁移（user_version=1，9 张表）、ONNX Runtime 随包加载日志、1280×720 与 1920×1080 布局检查、浅色/深色主题。

### 运行时资源（未纳入版本库）

`src-tauri/resources/` 下两个目录体积较大（合计约 1.7 GB），已从版本库排除；克隆后需按下述方式补齐，否则本地打标不可用（其余功能不受影响）：

- **`resources/onnxruntime/`**：ONNX Runtime 动态库。CPU/DirectML 版本取自 NuGet `Microsoft.ML.OnnxRuntime.DirectML`；CUDA 版本取自 `Microsoft.ML.OnnxRuntime.Gpu.Windows`，把 `runtimes/win-x64/native/` 下的 `onnxruntime.dll`、`onnxruntime_providers_cuda.dll`、`onnxruntime_providers_shared.dll` 复制进来。
- **`resources/cuda/`**：CUDA 12 与 cuDNN 9 运行库（`cudart64_12.dll`、`cublas64_12.dll`、`cublasLt64_12.dll`、`cufft64_11.dll` 与 `cudnn*64_9.dll` 全套），取自 CUDA 12.8 / cuDNN 9 官方发行包；应用启动时会把该目录注入进程 PATH 供 CUDA EP 加载，缺失时自动回退 CPU 并在日志中说明。

## 使用顺序

1. 在“API 连接”页添加供应商、Base URL 和一个或多个 Key；可从“预设服务商”栏一键填充知名服务商地址，或只填主机地址让应用自动补全；可先点击“测试连接”。
2. 回到“打标任务”，在 API 选择器旁的一行「LLM API / WD tagger」切换条中选择打标来源；切到“WD tagger”后同一位置出现 WD Tagger 模型选择与下载控件（预设高级设置中也有同样的入口），选择并下载后完全离线打标。
3. 选择单张、批量图片或文件夹，按需开启“包含子文件夹”。
   - 如需先清洗数据集，可进入“图片抽选 → AI 抽样”，填写保留要求、选择 API/模型和并发数，等待逐图判断后确认应用。
4. 编辑系统提示词和预设名称，在高级设置中配置生成参数、超时、并发与拒绝规则。
5. 开始打标。成功结果写入图片旁同名 `.txt`，默认不覆盖已有标签。
6. 切换“模型复核”后可将图片与当前标签发送给指定模型，修订结果直接覆盖并保留版本历史。
7. 使用“批量编辑”按五种范围执行 Tag 或纯文本规则；应用前预览，应用后可整批撤销。
8. 在“版本历史”恢复单文件任意版本或撤销整批；在“使用统计”按分组查看指标。
9. 在“Anima 参数”输入数据集图片数量、LoRA 类型和显存允许的 Batch Size；选择训练强度后复制生成的 Epoch、Repeat 与 TOML 片段，并按每个 Epoch 的样图决定是否提前停止。
10. 在“模型后处理”选择一个或多个 LoRA；确认检出的元数据字段后执行净化，使用同目录下的 `*.metadata-removed.safetensors` 作为最终模型文件。

## 数据位置与隐私

应用数据保存在 Tauri 的当前用户应用数据目录中（`%APPDATA%\local.lora.captionstudio`），**不在安装目录内**：

- `caption-studio.db`：连接的非敏感配置、预设、版本、任务与指标。
- `secrets.bin`：经 Windows DPAPI 按当前用户加密的 API Key 与敏感请求头值。
- `models\`：本地 WD tagger 模型。
- `backups\`：每日自动备份（数据库一致性快照 + 凭据文件，保留 7 天）。
- `logs\`：轮转运行日志，认证信息会被遮蔽。

**更新/重装不会丢失数据**：卸载仅清理安装目录，应用数据目录原样保留（已实测验证）；
若数据文件损坏（磁盘错误、外部清理等），应用会自动隔离损坏文件、以干净状态启动并保留损坏副本供恢复，
同时每日自动备份提供兜底。

图片只会发送到任务当时选中的 API 连接。未选择并启动任务时，应用不会上传图片。
