# taskloom 完成度评估

评估日期：2026-11-27。依据：当前源码、本地回归与最低版本验证；托管 CI 尚未通过验收。

## 下一轮交付进展

- 可复现任务评测已实现，覆盖研究、工具、审批和恢复，记录成本、耗时及失败原因；真实模型配置、凭证和费用上限仍待提供。
- 工作流与会话 API/UI 已接入，浏览器验收覆盖精确审批、角色交接、一次评审返工和连续两轮会话。
- 网页搜索/抓取、PDF 导入、向量与混合检索已实现。真实 Rust 官方网页导入与三处引用核对通过；真实 Linux Poppler 两页 PDF 提取和限额检查通过。模型仍为离线摘录 fixture，未验证付费搜索和真实向量模型效果。
- Linux 容器 Python 运行接口与容器内监督器已实现。用户启动 Docker 后，五项真实引擎验收通过，覆盖数值资源限额、权限/网络、脱离进程组的超时、输出限额、未关闭输入和取消清理；验收后无残留测试容器。它不等同于虚拟机级隔离或托管 CI 验证。
- GitHub Actions 账户因 billing issue 被锁，新预览的作业仍未开始。Git HTTPS 连接失败后，已通过 GitHub API 原样保留全部提交 SHA，发布开发分支、`v0.2.0-alpha.1` 标签和公开预览 Release；三个附件的 SHA-256 已核对，匿名访问及源码下载校验通过。发布成功与 CI 未运行分别记录。
- `0.2.0-alpha.1` 已完成打包编译、静态资源检查和全新 release 安装；安装后的版本正确，能完成两份资料导入、引用核对和不重放的完成后恢复。见[预览版](https://github.com/seriable/taskloom/releases/tag/v0.2.0-alpha.1)。

逐项验收状态以 [交付清单](preview-delivery-roadmap.md)、[研究工具](research-tools.md) 和 [沙箱边界](sandbox.md) 为准。以下 v0.1/前三阶段内容保留为基线记录。

## 结论

作为 README 定义的“类型化 LLM 任务执行库”，v0.1 的主要功能和已发现的发布阻塞修复已经落地。
这是 GitHub 上的早期开放源码版本，适合业务集成和受控试用；真实 Provider 兼容性和生产规模验收
仍需在使用者的目标模型及部署环境中进行。CI 状态以仓库 Actions 中的实际运行结果为准。

默认 API 的 Agent 负责执行单个结构化任务，调用方决定任务间的业务流程。
开发分支新增可选 `runtime` 层：原生工具消息、工具注册与权限、受限 AgentLoop、
事件轨迹和运行级模型预算。它不改变单任务 API，也不复用单任务答案缓存来执行工具。
新增 `DurableAgent` 提供检查点、暂停恢复、人工审批、预算日志和不确定副作用核对。
会话和上下文压缩已接入持久化运行，保留完整历史并限制模型输入；
多 Agent 工作流已具备顺序/并行/条件 DAG、稳定子运行、全局预算预留和有界评审返工，
三个阶段均已通过本地开发验收：第三阶段补齐可插拔知识检索与引用核对、
MCP Streamable HTTP、可安装 CLI、带认证的本地 API 和运行/审批 UI。
这是单操作员的预览框架；真实模型兼容性、托管 CI 和生产规模验收仍需单独完成。
详见 [开发清单](agent-roadmap.md)、[运行时指南](agent-runtime.md)、[恢复指南](durable-runs.md)、
[协作指南](workflows.md) 和 [安装/API/操作指南](application.md)。

## 本次已修复

| 问题 | 修复后的行为 | 回归覆盖 |
|---|---|---|
| 不同采样参数复用旧答案 | 请求键包含格式、temperature 精确位值和输出 Token 上限 | 缓存隔离、相同参数命中、各参数独立回放、未知参数离线失败 |
| 无效响应漏计费用 | 网关先保留响应 usage，再验证结果；失败分类、重试规则保持生效 | 无效 JSON、拒答、结构化重试及文本回退、Embedding 数量错误 |
| 网关降级后报告原模型 | `Response` 携带实际服务模型，缓存和录制文件保存 `served_model` | 普通降级、修复、hedging、引用和 Arc 包装、缓存及离线回放 |
| OpenAI 拒答被当作 JSON 错误 | 普通与流式拒答信号统一分类为内容拒绝，保留已报告用量 | 拒答字段、内容过滤、无需协议重试/修复、拒绝录制或缓存 |
| 连接关闭被当作流式成功 | OpenAI 要求 `[DONE]` 或非空结束原因，Anthropic 要求 `message_stop` | 有效 JSON 提前断流仍失败；正常结束保留后续用量，完整答案可缓存和回放 |

新增 API 使用默认方法兼容已有 `Transport::send` 实现。
有内部路由的自定义 Transport 应覆盖 `send_with_metadata`，明确返回服务模型。
`TaskOutcome::models_tried` 统计任务声明的外层模型链，不统计网关内部尝试次数。

请求键修正会改变旧的聊天录制和缓存文件名，需要重新录制；不会为兼容旧键而误用旧答案。
回放缺少条目时仍直接返回 `NotRecorded`。在当前请求键下，未写 `served_model` 的记录
会使用请求模型作为兼容默认值。

另修复了默认 feature 文档对可选 OpenAI 模块的无效链接，并将各 feature 文档检查加入 CI。
Transport 选项取值也改为显式 `as_any`，消除了源码对较新 trait upcasting 功能的依赖。

## 已具备的主要能力

- 类型化任务、角色解析、任务模型链、结果校验和修复。
- Resilient JSON、整体任务 deadline、单次尝试超时、取消和 hedging。
- Provider 并发控制、模型熔断、瞬时错误重试及模型降级。
- 单请求输入 Token 预算、价格表、调用账本和 tracing。
- 录制/回放、校验后步骤缓存、Embedding、基于录制样本的评估。
- OpenAI-compatible 和 Anthropic 适配器，以及本地 HTTP 流式测试。

## 发布收尾结果

| 原优先级 | 原问题 | 当前实现与验证 |
|---|---|---|
| P1 | Rust 最低版本与依赖不一致 | 声明与 CI 统一为 Rust 1.86，锁文件固定依赖；CI 实际执行最低版本测试 |
| P1 | Embedding 降级可能混合向量空间 | 禁止自动跨模型降级；测试部分缓存命中、主模型失败、恢复后补齐；额外验证向量非空、有限和维度一致 |
| P1 | 缓存及录制并发写入竞争 | 跨进程目录写锁、唯一临时文件、刷盘与原子替换；Windows 暂时共享冲突有上限重试；多进程连续写入时读取完整 JSON 的测试覆盖 |
| P1 | 注册表超时未生效、排队不计时 | 注册表 timeout 约束整体任务，显式 deadline 只能缩短；单次尝试包括 admission；暂停时间测试验证排队、退避和更短 deadline |
| P2 | 费用统计与全局金额限额容易混淆 | README 明确限定为输入 Token 预算和已报告费用核算，移除对全局费用硬上限的暗示；全作业金额预算不属于 v0.1 的能力 |
| P2 | 跨端点缓存误用 | 内置传输按端点、协议和路由配置命名空间隔离；自定义 HTTP 客户端或租户可显式指定命名空间；共享目录下缓存和回放均有测试 |

录制失败现在返回 `TaskError::Storage`，停止任务且不重试其他模型。
可选步骤缓存仍为尽力写入，失败会记录 warning。原子存储验证不等同于大规模吞吐压力测试。
自定义 Transport 应实现 `cache_namespace`；同一端点的租户或自定义客户端路由差异，
应通过 `GatewayTransport::with_cache_namespace` 声明，命名空间中不能包含凭据。

## 本地验证与限制

验证环境：Windows（Rust/Cargo 1.99 和真实 1.86）；WSL Debian Linux（Rust/Cargo 1.95）。

- 单任务基线为 101 个测试；前三阶段验收为 195 个测试（含多进程测试子入口）。新增评测、操作入口和研究/沙箱回归已扩展此基线，环境相关验收单独报告，忽略的测试不计为通过。v0.1.0 标签对应此前的 94 个测试。
- 默认空 feature、openai、anthropic、schemars、runtime、runtime + openai、runtime + anthropic、mcp、app 九种组合分别验证测试和文档。
- 格式检查、全目标/全 feature Clippy（warnings 视为错误）。
- 全 feature 和上述各单 feature 文档构建（warnings 视为错误）。
- basic、record_replay、production、eval、agent_loop、agent_resume、agent_session、agent_peer、agent_research 九个离线示例。
- 实际完成本地 debug profile 的 CLI 安装；安装后的程序能执行知识任务并核对引用。crate 打包和包内编译验证通过。
- 浏览器验收覆盖任务创建、重启后查看结果、引用核对和审批后恢复真实文件写入；窄屏页面无横向溢出。

新增测试位于 `tests/gateway_regressions.rs`、`tests/reliability.rs` 和 `tests/step_cache.rs`。
Provider 协议回归测试位于 `src/openai.rs` 和 `src/anthropic.rs` 的测试模块。
运行时回归测试位于 `tests/agent_runtime.rs`；`tests/agent_runtime_http.rs` 用本地 HTTP
验证两个原生 Provider 的完整工具往返，包含真实结果的下一轮消息。新增 Schema 依赖的 UUID
版本同时在清单中约束，避免下游解析到要求 Rust 1.89 的版本，继续支持 Rust 1.86。
`tests/agent_durable.rs` 包含进程实际退出后的恢复、审批绑定、幂等重试去重、写入失败、
取消、预算日志和原子检查点验证。锁的最后持有者会显式解锁；重复句柄回归与 Linux
连续十轮恢复测试覆盖了运行返回后锁短暂残留的问题。
`tests/agent_context.rs` 验证有损摘录压缩、工具关联、字节限额和恢复后复用原模型输入；
`tests/agent_session.rs` 验证多轮会话、并发绑定、审批占用和失败后的可审计关闭。
工作流回归覆盖有界并行、条件分支、精确交接、全局预算预留、有限评审返工、
子进程实际退出和父检查点故障后的安全恢复。RAG 回归覆盖 UTF-8 原文范围、
原子替换/CAS、伪造引用拒绝和跨检索版本保留；MCP 使用本地 HTTP/SSE 协议夹具。
应用回归包含真实 HTTP、实际 CLI 子进程、认证、审批、暂停取消、工作线程 panic 和重启恢复。
运行状态格式更新为 2，开发中的格式 1 不会被静默迁移或重置；单任务录制格式保持不变。
GitHub CI 配置覆盖 Linux/Windows、真实 Rust 1.86 编译测试、feature 矩阵、示例和打包。
当前托管 CI 尚未完成验证；发布验证通过的是本地 Windows、真实最低 Rust 版本，以及独立 Linux 环境。
不能把本地通过的结果描述为 GitHub CI 已通过。
本次没有调用收费的真实模型接口。测试通过不能保证所有第三方兼容接口均接受同一组参数。

真实接口验收应覆盖：普通 JSON、Schema、流式结束/中断、拒答、429、输出截断和取消，
并核实 Anthropic 版本头、各模型采样参数及 API 字段兼容性。
使用量只能记录 Provider 实际返回的数据；被取消的请求或未完成的流仍可能在服务端产生费用。

v0.1 是 GitHub 预览版，仅发布源码和版本标签，尚未发布 crates.io 包；安装示例使用固定 Git tag。
上述新增框架能力保存在开发分支，尚未发布到旧标签；应用安装需使用包含这些代码的 checkout。
准备投入生产前，应在目标 Provider 上完成小规模联调及对应工作负载的压力与恢复测试。
