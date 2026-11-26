# taskloom 完成度评估

评估日期：2026-11-27。依据：当前源码、回归测试、最低版本验证和 GitHub CI。

## 结论

作为 README 定义的“类型化 LLM 任务执行库”，v0.1 的主要功能和已发现的发布阻塞修复已经落地。
这是 GitHub 上的早期开放源码版本，适合业务集成和受控试用；真实 Provider 兼容性和生产规模验收
仍需在使用者的目标模型及部署环境中进行。CI 状态以仓库 Actions 中的实际运行结果为准。

项目的 Agent 负责执行单个结构化任务，调用方决定任务间的业务流程。
自主规划、工具调用循环、记忆和多 Agent 协作不属于当前设计范围；
如果产品需要这些能力，应在任务执行层之上单独定义编排层，不能把当前库视为已经具备这些能力。

## 本次已修复

| 问题 | 修复后的行为 | 回归覆盖 |
|---|---|---|
| 不同采样参数复用旧答案 | 请求键包含格式、temperature 精确位值和输出 Token 上限 | 缓存隔离、相同参数命中、各参数独立回放、未知参数离线失败 |
| 无效响应漏计费用 | 网关先保留响应 usage，再验证结果；失败分类、重试规则保持生效 | 无效 JSON、拒答、结构化重试及文本回退、Embedding 数量错误 |
| 网关降级后报告原模型 | `Response` 携带实际服务模型，缓存和录制文件保存 `served_model` | 普通降级、修复、hedging、引用和 Arc 包装、缓存及离线回放 |

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

验证环境：Windows，Rust/Cargo 1.99。

- 全 feature：75 个单元测试、19 个集成测试，共 94 个（含多进程测试子入口）。
- feature 组合：无默认 feature、openai、anthropic、schemars 分别运行测试。
- 格式检查、全目标/全 feature Clippy（warnings 视为错误）。
- 全 feature 和上述各单 feature 文档构建（warnings 视为错误）。
- basic、record_replay、production、eval 四个离线示例。

新增测试位于 `tests/gateway_regressions.rs`、`tests/reliability.rs` 和 `tests/step_cache.rs`。
GitHub CI 覆盖 Linux/Windows、真实 Rust 1.86 编译测试、feature 矩阵、示例和打包。
本次没有调用收费的真实模型接口。测试通过不能保证所有第三方兼容接口均接受同一组参数。

真实接口验收应覆盖：普通 JSON、Schema、流式结束/中断、拒答、429、输出截断和取消，
并核实 Anthropic 版本头、各模型采样参数及 API 字段兼容性。
使用量只能记录 Provider 实际返回的数据；被取消的请求或未完成的流仍可能在服务端产生费用。

v0.1 仅发布 GitHub 源码和版本标签，尚未发布 crates.io 包；安装示例使用固定 Git tag。
准备投入生产前，应在目标 Provider 上完成小规模联调及对应工作负载的压力与恢复测试。
