# 隔离 PNG OCR SDK

`ocr` 是单独启用的 feature（包含 `sandbox`），提供
`runtime::ocr::{DockerOcr, DockerOcrConfig, OcrLanguage, OcrOutcome}`。
它只接受宿主传入的图像 bytes，运行固定的 Tesseract 程序，返回可审核的文本快照。
没有新增模型工具、网页上传路径、HTTP 任意文件接口、云模型请求或宿主 OCR fallback。
旧 Python `CodeSandbox` 的 64 KiB 代码合同、镜像身份和文件上限保持原样。

## 输入与固定程序

宿主固定本地 Linux Docker engine/socket、Docker CLI、审阅后的镜像 digest、平台、
语言及资源限制。语言只有 `english`、`simplified_chinese`、
`simplified_chinese_english`；程序、traineddata 路径、输入文件、输出 stdout、
PSM 6、OEM 1 和 DPI 300 都是固定合同，图像不能选择代码、shell、程序、路径或参数。

当前支持单帧 PNG 子集：不超过 4 MiB、16 Mi 像素、任一边不超过 16384，结构完整、
chunk CRC 正确、IHDR/IDAT/IEND 顺序有效；不支持 APNG、JPEG、PDF、TIFF、OCR
布局坐标或通用视觉理解。宿主用纯 Rust 检查 framing/header/CRC，不进行原生图像解码。
完整图像只经 stdin 发送，argv 仅带准确长度和 SHA-256。PID 1 在接收 stdin 前开始
计时，未 EOF、空输入、部分帧、长度/摘要不符都不能进入 Tesseract。

## 独立隔离合同

新 worker 使用 `sandbox/ocr/Dockerfile` 与 `supervisor.py`，必须审阅、构建后固定
最终镜像 ID/digest。运行使用 `--pull=never`，不自动拉镜像或安装包。构建允许从
Debian 官方发行包仓库或宿主选定镜像下载 Tesseract/eng/chi_sim；apt 发行签名验证保持
开启。Dockerfile 默认官方仓库，提供 `DEBIAN_MIRROR` / `DEBIAN_SECURITY_MIRROR`
构建参数。发行包更新会改变构建结果，不能假定同一 Dockerfile 永远生成同一镜像。

每次创建独立容器：无网络、无宿主 bind mount/socket/凭证、只读 rootfs、cap-drop ALL、
仅 SETUID/SETGID、no-new-privileges、标准 seccomp、私有 IPC/cgroups、32 PID、受限
CPU/内存且 swap 上限与内存相同、64 个文件描述符、4 MiB 单文件上限和零 core dump。
宿主 Docker 环境清空，使用私有空配置目录，不能注入代理、registry 凭证或 context。
引擎能力核验失败则拒绝执行，不忽略内存/PID/CPU/seccomp 限制失败。

root Python monitor 只处理受限 PNG 结构/CRC/SHA、固定模型清单和 I/O，不用原生库
解码图像。`/tmp` 是 16 MiB root-owned 0711 的 non-executable tmpfs；输入是 root-owned
0444 的固定 `input.png`，child 可读而不可替换；仅独立 01777 工作目录可写。
Tesseract 作为 uid/gid 65534、无补充组的新 session child 运行，固定单 OpenMP 线程。
原生 PNG/图像解析漏洞被限制在该降权容器进程中；容器仍共享内核，不是 VM 安全保证。

root monitor 同时读取 stdout/stderr，限制文本、错误流及合计字节，监督墙钟 deadline
（最多 30 秒），超时/超量立即退出 PID 1，Linux runtime 结束剩余 namespace 进程。
最终结构化响应也只有 500 ms 非阻塞写入宽限；失去宿主读取方不能令容器长期存活。
响应使用 base64，Rust 严格验证 UTF-8，不用有损替换掩盖文本 SHA-256。

## 结果、清理与来源

每次容器名带独立随机 nonce，同一 context 并发调用不会通过 cleanup 杀掉彼此。
返回时强制清理准确的命名容器，再用 Docker inspect 核验不存在；daemon 不可用、
拒绝或结果不确定就保留 `cleanup_confirmed:false`。异常诊断包含容器名供宿主检查。
调用者取消后，清理由仍存活的 Tokio runtime 异步 best effort 执行；取消没有清理完成
回执。宿主/runtime/daemon 崩溃可能阻止清理，不应把进程退出等同于资源已移除。
PID 1 自监督墙钟仍依赖正常工作的 Linux runtime。资源上限是每次调用限制，不是全局
Docker 并发或费用额度。

只有成功、完整且确认清理的结果可用 `outcome.into_document(id, title)` 转为 Document。
provenance 包含原 PNG 全字节 SHA-256、长度/尺寸、worker 镜像及平台、语言、固定
manifest SHA-256、Debian Tesseract package 版本、eng/chi_sim traineddata SHA-256 和
提取文本 SHA-256。模型文件在容器中校验；源码仓库不提交 traineddata、字体或镜像。
原图任何字节改变均改变来源绑定，即使 OCR 输出相同。

`Document.source` 标注 OCR 快照来源，offset/citation 只精确指向**提取的 UTF-8 文本**。
Tesseract 会漏字、错字或误判布局；引用不是图像逐字准确、坐标或事实正确的证明。
失败、部分输出、空文本或清理未确认的结果不创建 Document。

```rust,ignore
let worker = DockerOcr::new(host_config)?;
let outcome = worker.extract(&host_supplied_png_bytes, &tool_context).await?;
// 宿主先检查 outcome 审计字段；没有隐式写入知识库。
let document = outcome.into_document("scan-1", "已审核 OCR 快照")?;
```

## 本地验证

普通测试验证固定 argv、无 mount/network、随机名称与稳定 identity、PNG 损坏/CRC/像素
bomb/超长拒绝。真实容器测试需显式设置 `TASKLOOM_TEST_DOCKER`、
`TASKLOOM_TEST_DOCKER_ENDPOINT`、`TASKLOOM_TEST_OCR_IMAGE`，并运行
`cargo test --locked --features ocr --test agent_ocr -- --ignored --nocapture`。
测试使用忽略的 `target/ocr-fixtures` 内受控英文、简中和长页 PNG，生成脚本为
`examples/ocr/create_fixtures.py`；它只渲染本机可信字体，不解码提交的图像，也不复制字体。
设置 `TASKLOOM_TEST_OCR_FIXTURES` 为该目录的绝对路径。生成图像、字体、traineddata
和镜像均不提交源码仓库；普通测试在内存中构造最小 PNG 验证 framing。
真实测试核对中英内容、原图/文本/model hash、输出上限、timeout、损坏原生解码、未 EOF
及半帧 ingress、实际取消和准确命名容器移除。测试不代表扫描件准确率基准或视觉模型能力。

宿主 CLI 示例为 `cargo run --features ocr --example agent_ocr -- CONFIG_JSON INPUT_PNG ID TITLE`。
它只读取明确选择的有界本地配置/PNG，输出结果审计及可用时的 Document JSON，不自动写库，
不向服务器添加路径读取接口。

本轮受控本机识别与资源边界的源码/模型摘要见
[2026-11-28 验收元数据](acceptance/ocr-20261128.json)。生成 PNG 和完整 CLI 输出留在
本机忽略目录，元数据不包含原图、字体、traineddata、凭证或可执行镜像。
