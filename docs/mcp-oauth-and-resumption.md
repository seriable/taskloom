# MCP OAuth 与显式 SSE 恢复

这两项是 `mcp` SDK 的独立 opt-in，不会改变旧 `McpClient::connect` 的默认能力
或稳定工具指纹。可用 `connect_with_oauth` 单独交接 OAuth 凭证，或用
`connect_with_extensions(config, McpHttpExtensions { oauth, interactions, resumption })`
组合已审核的宿主交互与恢复策略。HTTP OAuth 与 App 用户 SSO 登录是不同合同。

## 固定 OAuth 公共客户端

`McpOAuthConfig` 是宿主填写的公开策略，固定 `resource`、资源 metadata URL、
`issuer`、授权服务器 metadata URL、授权/token endpoint、`client_id`、回调 URI
以及 scopes。地址必须是规范的 HTTPS URL，不能带用户信息、query 或 fragment；
显式 `allow_http_loopback_for_tests` 只为本地 HTTP 夹具放宽。生产配置不要开启它。
所有请求禁用跳转和环境代理，读取受总请求时限与响应大小上限约束。

`McpOAuthClient::discover(config)` 只 GET 宿主固定的两个 metadata 地址。
资源文档必须精确声明固定 resource 和唯一固定 issuer，授权文档必须精确声明同一个
issuer、authorization endpoint 和 token endpoint，支持固定 scopes、authorization_code、
code response、PKCE S256、public client `none` 和回调 `iss` 参数。
任何不匹配都会拒绝；不会跟随 MCP 服务器给出的 WWW-Authenticate URL、注册端点、
其他发行者、登录链接或令牌重定向。

宿主调用 `client.begin()` 得到 `McpOAuthAuthorization`。`authorization_url()`
只用于可信宿主的浏览器交接，包含独立随机 state、PKCE S256 challenge 和 RFC 8707
resource 参数，不应放入模型工具内容或日志。框架不会自动打开浏览器或启动回调服务；
宿主负责路由这次交互的真实浏览器回调。

`flow.exchange(callback_url).await` 消耗不可 Clone/Deserialize 的 owned flow，先检查
有效期、完整回调地址、唯一 code/state/iss、精确 state 和 issuer，然后仅向固定 token
endpoint 发送一次授权码、verifier、public client ID、回调 URI 和 resource。无论失败、
超时还是取消，都不能重用该 flow 自动交换；不接受客户端密钥、refresh token 或隐式登录。

响应要求有界 Bearer access token、与请求完全相同的 scopes 和 1–86400 秒有效期。
有效期保守地从 token 请求开始计算；慢响应不能延长其窗口。返回的
`McpOAuthCredential` 不实现 Debug/Serialize，不提供原始 token getter，也不会落盘。
交给连接时 resource 必须与 MCP endpoint 字符串完全相同，且不能覆盖已有 bearer。
到期后连接拒绝新的请求，不自动刷新或重发工具。宿主必须保护浏览器 URL、回调和进程内存。

issuer/resource pin 和 RFC 8707 参数固定的是本次授权的目的地。**本客户端没有对
opaque access token 的 aud 做本地密码学验证**；资源服务器仍须校验 token 的实际
issuer/audience/scope。认证成功不会创建 Taskloom 工具权限、跳过原审批或增加费用额度。
公开合同进入新工具身份，token、state、verifier 和每次连接随机数不进入稳定工具指纹。

opaque access token 也不能向 SDK 证明 subject/principal。不同登录用户若共用相同
resource、client、scopes 和 `McpHttpConfig.namespace`，其稳定工具身份可能相同。
宿主必须用 `namespace` 固定非秘密的租户/主体授权域；切换主体时使用新的 namespace、
连接配置和运行 ID，不能将旧运行的授权或恢复记录继续用于另一个用户。
同一主体的 token 轮换不等于主体切换；也不能从 resource 参数、PKCE 或登录成功推导
用户隔离已经得到验证。主体识别与授权域绑定仍由可信宿主和资源服务器落实。

## 只对原请求执行显式 GET

宿主在 `McpHttpExtensions.resumption` 设置 `Some(McpSsePolicy::default())`，
才会为有原 session 的 `tools/call` 保留内存恢复记录。注册与容量检查发生在原 POST
之前；默认最多 16 条 pending、每个原请求最多 1 次显式 GET、最多 256 个有编号事件，
单个 event ID 最多 256 字节。策略可在文档化的硬上限内调整。

原请求断流、超时或取消后，`pending_resumptions()` 返回非活动的 opaque
`McpSseResume`。句柄绑定本连接实例、endpoint/命名空间/公开认证合同、原 session、
协议、request ID、原请求摘要和最后处理完成的 SSE event ID。没有完整 event ID 的记录
表示原结果仍不确定，不能发送恢复 GET。event ID 只接受非空可打印 ASCII，不能含空格、
控制字符或重复 ID；不会因为重放事件再次触发宿主回调。

`client.resume_tool_call(&handle).await` 是显式宿主动作。它先占用一次恢复机会，然后
仅向同一固定 MCP endpoint 发送 GET，带原 `Mcp-Session-Id`、协议和 `Last-Event-ID`。
只接受有限 SSE 中与原 request ID 匹配的结果。不会生成新请求 ID、自动 GET、断线重连
或重新 POST `tools/call`。服务端 404/拒绝恢复、错 session、错 request、超时、重复事件
都保持失败与原结果不确定，不用新工具调用伪装恢复。
最终事件必须先通过 JSON-RPC 和工具 content/isError 完整校验才能提交游标/删除记录；
恶意或缺字段的最终结果保留此前游标与 pending。完整合法的工具失败结果会结束该记录。

新事件处理完成后更新游标；如果宿主明确允许多次 GET，必须取新的句柄继续。
旧游标、其他连接的句柄和并发占用会在网络请求前拒绝。回调未处理完就取消的事件
不会提前推进游标；`McpInteractions` 自身也阻止同一个服务器 request ID 再次调用宿主。
成功取回结果后删除记录，`discard_resumption` 仅丢弃本地记录，不取消远端工作。

这些记录是**单进程内存状态**：不持久化 token、请求正文或 event ID，不支持退出/重启
恢复、多机协调、后台独立请求流，也不能替代运行时的 durable reservation、审批或
`NeedsRecovery` 人工核对。SDK 取回原结果并不自动修改运行时恢复记录；宿主必须按原
恢复流程核对后再处理。关闭原连接后不能继续恢复。

## 验证边界

`agent_mcp_oauth` 使用真实 loopback metadata、PKCE/token HTTP 交换与带鉴权的 MCP
初始化/调用，验证 discovery pin、回调来源、scope/type/expiry、资源绑定、交换失败无重试，
以及慢 chunked token 的保守到期。没有真实发行者认证或跨厂商 OAuth 互操作认证。

`agent_mcp_resume` 使用真实 HTTP/SSE 断流，验证原效应 POST 计数始终一次、显式 GET、
无游标/default 拒绝、错 session/request、重放 event、过期句柄、单连接隔离、容量前置、
调用者取消与新游标恢复。不声称持久化交付或 exactly-once 执行。

需要进程重启/多 owner 协调时，另见独立 feature 的
[受保护 SQL 持久恢复 SDK](mcp-durable-recovery.md)。它不复用这里的内存句柄；v1 不允许
组合 OAuth helper/sampling/elicitation 扩展，凭证由宿主重新注入，不自动核销运行时恢复记录。
