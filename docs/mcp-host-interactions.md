# MCP 主机审核交互

`mcp` feature 提供显式启用的 SDK 回调：`McpClient::connect_with_interactions`
和 `connect_stdio_with_interactions`。原有 `connect` / `connect_stdio` 的能力声明、
默认绑定身份保持原样，不自动开启 sampling 或 elicitation。

这里实现的是有来源绑定和边界检查的协议交接。框架不会自动选择模型服务、读取密钥、
调用付费模型、收集表单答案或打开登录链接，也没有自动连接 App 审批页面。

## 主机合同

宿主创建 `McpInteractionPolicy` 和 `Arc<dyn McpInteractionHandler>`，再用
`McpInteractions::new(policy, handler)` 固定该合同。`identity()` 必须返回稳定、
不含密钥的版本标识；策略或该标识改变时，启用交互的连接身份和工具指纹改变。
连接期间标识改变会拒绝交互。

收到符合策略的请求后，先调用 `approve(&McpInteractionRequest)`。宿主审核请求的
`method()`、不可信的 `params()` 和 `binding()`，显式返回
`McpInteractionApproval::allow(request.binding(), actor, note)` 或 `deny()`。
只有完全匹配的允许决定才能进入 `respond(request, approval)`。允许值不能从
服务端 JSON 反序列化。来源绑定包括连接、命名空间、外层工具请求、协议版本、
服务器原始请求和固定交互合同；不能拿另一次请求的决定复用。

回调是可信宿主代码。若 `respond` 要执行模型/API 调用，宿主必须在该调用之前落实
原有持久化费用预留、权限和不确定结果恢复规则。此 API 的允许决定和内存日志
不能替代这些规则。取消 Future 也不能证明宿主外部副作用没有发生。
服务器请求、描述、模型偏好及表单说明始终是不可信输入。

## 支持范围

只处理一个已关联 `tools/call` 响应过程中的服务器请求；初始化、工具发现和资源
请求不能触发宿主回调。HTTP 支持有限 SSE 响应内的请求，回复以带会话/协议头的
独立 JSON-RPC POST 发到同一个固定 MCP 地址，并要求 HTTP 202；不会重发外层
`tools/call`。stdio 在同一个直接子进程的 stdin/stdout 上发送关联回复。

- `sampling/createMessage`：必须显式固定 `sampling_model`，只接受有界的纯文本
  user/assistant 消息、`includeContext` 缺省或 `none`，且 `maxTokens` 不超过宿主
  上限。服务器的 `modelPreferences` 不会选择宿主模型。返回值必须是该固定模型
  标签的 assistant 文本。实际模型的 token 上限由执行模型调用的宿主另行落实。
- `elicitation/create`：仅支持协商 `2025-06-18` 后的普通表单。宿主提前用
  `schema_fingerprint` 校验并固定完整 schema，再在 `elicitation_schemas` 为该摘要
  登记字段名白名单。schema 必须是禁止额外字段的 object，仅允许基本类型字段；
  不接受引用、嵌套对象/数组、URL 登录或 password/secret/token/api-key 类字段名。
  接受的回复按原 schema 校验；也可以明确 decline/cancel。

字段名过滤是有限的防误用规则，不能识别所有语言、暗示或隐藏的敏感信息。
宿主仍须审核每次提示、schema、字段含义和答案，不应收集凭证。

## 限制、审计与恢复

默认每个显式连接最多接收 16 个有效编号的服务器请求（可配置 1–256），
单条请求/回复默认 16 KiB（可配置 512 B–1 MiB），单次回调默认 5 秒（最多 60 秒）。
传输自身的帧、响应和外层请求总时限仍然生效。重复请求 ID 不会再次调用宿主；
连接达到请求数上限会失败，重新连接只能由宿主显式发起。

`client.interaction_audit()` 返回本连接内存中的无提示词事件快照。进入审核前先写入
`pending_host_review`，允许后变为 `approved_pending_response`；本地校验回复成功
只标记 `response_prepared_not_delivery`，不声称对端已收到。回调时限触发会记录
`timed_out_callback_outcome_unknown`。外层取消或超时可能留下前两种 pending 状态，
仍然保留已经开始的审核/回复证据，不自动重试。宿主需要自行持久化所需审计记录；
进程退出后本 API 不提供可恢复审计、费用结算或 exactly-once 交付保证。

OAuth 公共客户端和单进程显式 SSE GET 恢复有另外的
[opt-in 合同](mcp-oauth-and-resumption.md)，不会由回调自动开启。尚未实现 URL
elicitation、独立后台服务器请求流、自动断线重连、自动模型 sampling 后端或跨进程/
分布式交互恢复。

## 验证

`cargo test --locked --features mcp --test agent_mcp --test agent_mcp_interactions`
使用实际本地 HTTP/SSE 连接和由 rustc 编译的真实子进程，验证默认关闭、来源绑定审核、
纯文本 sampling、固定 schema 表单、重复 ID、协议/上下文/token 限制、秘密字段/URL
拒绝、回调超时、外层取消留下的 pending 证据及服务器请求数上限。没有调用真实模型，
这些测试不等于所有第三方 MCP 服务的互操作认证。
