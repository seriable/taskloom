# SQL 持久 MCP 恢复 SDK（显式启用）

`mcp-persistence = ["mcp", "database"]` 提供独立的
`PersistentMcpClient` / `SqlMcpSessionStore`。旧 HTTP/stdio API、默认 `[]`、工具身份与
单进程 SSE 恢复不变。当前是宿主 SDK：没有自动注册工具、HTTP 管理路由或运行时恢复核销。
只支持原 HTTP session 的有限 SSE；不恢复 stdio 子进程，不创建替代 session。

## 宿主固定授权域与保护 SQL

宿主先连接并迁移受保护的 `SqlDatabase`，用固定 tenant/project 建立
`SqlMcpSessionStore`，再用 `McpPersistentConfig::new(http, principal_namespace)`
提供固定 endpoint、公开 namespace/principal 授权域与资源限制。生产仅接受 HTTPS；
loopback HTTP 必须显式设置测试选项。URL 禁止用户名/密码、query、fragment。
请求禁用重定向、环境代理与自动重试。

`McpHttpConfig.bearer_token` 仅由宿主在本次进程中注入；**不写入 SQL**，不自动刷新。
`open` 时宿主重新提供同一授权域的凭证。opaque token 不能向 SDK 证明 subject；宿主
必须把非秘密的真实租户/主体授权域固定在 `principal_namespace`，换主体时更换配置、
namespace 与 run，不能借相同 endpoint/client/scopes 或 token 轮换推断用户隔离。
服务端仍负责验证 bearer 与原 session 的授权关系。

SQL 内部会保留敏感的 session header、事件游标和已观察结果。必须使用宿主私有数据库
权限/目录，并保护数据库、WAL、日志及备份。相关内部 serde 不公开为导入格式；client、
config、store、handle、observed 类型无 Debug/Serialize，且不提供 session header getter。
`PersistentMcpSummary` 只给出请求 ID、绑定摘要、状态、是否有游标、GET 次数及响应摘要，
不包含 endpoint、header、原参数、回调文本或结果正文。宿主显式读取 `response()` 才获得
已观察的原响应，该内容仍是不可信数据，不能自动变成权限或指令。

v1 **在数据库/网络操作前静态拒绝全部 `McpHttpExtensions`**，包括 sampling、elicitation、
OAuth helper 和单进程 resumption。初始能力声明不包含 sampling/elicitation。收到服务端
callback 会先持久记录 `blocked_callback_unknown` 再失败；不调用宿主 handler、不调用模型、
不发送 callback reply。不能以重新 sampling 或重复回复伪装恢复。

## 持久状态与首次调用

`create(config, store, logical_name)` 是一次明确的初始建连。SQL 先保留逻辑 session 名和
Initializing 记录，再执行初始化/有界工具发现；成功后固定原 session header、协议、工具
catalogue、next ID 和公开配置绑定。bootstrap 确认丢失或没有 server-assigned session 时，
记录保持不可恢复的初始化不确定状态；`open` 不会再次 initialize，`create` 不覆盖旧记录。

`call(remote_name, arguments, ToolContext)` 只使用已发现的工具名。宿主仍负责原工具权限、
审批及外部服务费用；SDK 不从服务端 annotations 获得执行授权。context 的 run/call ID
只以摘要保存，参数只保存绑定摘要，不保留原始参数。相同 context 换参数或工具会拒绝。

一次调用先在 fenced SQL transaction 中增加 next ID，记录
`post_outcome_unknown` 及原 wire 请求绑定，确认提交后才允许**这一次首次 tools/call POST**。
任何失败、超时、取消或再次进入都不重 POST。已有 Observed 可读取；unknown 只能通过明确
恢复/放弃处理。每个 session 只允许一个未解决调用，所有本地操作串行进入。

有效 SSE 数据事件处理完成后才保存游标与 seen ID；无效/重复 ID、错误响应版本、错误
request ID、缺失 content/isError 等都不能推进最终游标。整个最终响应先完整验证，再把
`observed_result`、原 JSON 响应、其规范 JSON SHA-256 与最终游标**一次 SQL 提交**保存，
确认后才向宿主返回。进程在返回后、RunStore 保存前崩溃，结果仍然保留。

## 接管、ACK 与不确定结果

SQLite 用于同机多进程协调；PostgreSQL 使用数据库时钟的续租/owner/epoch 支持多个
进程的所有权协调。只有持有当前 SQL lease 的 owner 能提交状态。过期旧 owner 的写入
被 fencing 拒绝；租约不能撤回已到达远端的 POST 或让普通 MCP 服务器理解该 epoch。
这不是全局 exactly-once、远端执行取消或所有服务都能恢复的保证。

`release_owner()` 只放弃本地 lease，**不 DELETE 原 remote session**。释放在正常 runtime
内异步完成，否则由数据库 TTL 决定接管时间。`open` 只读取原 SQL 状态并申请新 epoch，
不发送 initialize、catalogue 请求或新 tools/call。调用 `pending()` 取得保留记录及 opaque
handle；其中也包含 ACK/放弃后的 tombstone，状态决定是否可恢复。

`resume(handle)` 对 Observed 直接读回 SQL；对 unknown 必须存在原完整游标，并在 SQL
先占用一次 GET 次数，然后仅向原 endpoint 发带原 session/protocol/Last-Event-ID 的显式
GET，仍验证原 request ID。没有游标、404、401、session 改变、结果损坏、GET 上限、
callback 或 owner 丢失都保持失败/unknown。不会切换 endpoint、初始化替代会话或重 POST。
远端不保存事件时，SDK无法制造丢失结果。

若 Future 在 SQL 保存期间取消，或数据库提交确认失败，owner 会被标记为本地 commit
unknown，拒绝后续操作。宿主必须 `release_owner()` 后重新 `open` 获取新 epoch，排序在
此前提交/回滚之后；不能沿用可能过时的内存状态派发第二次请求。

宿主把 `observed.response()` **先持久采用到自己的 RunStore**，再调用
`acknowledge(handle, observed.sha256(), adoption_reference)`。ACK 必须匹配完整响应摘要，
逻辑删除大结果/游标并保留绑定、摘要与 adoption 摘要 tombstone。返回/成功 GET 都不会
自动 ACK。独立 RunStore 与 SQL journal 之间没有跨存储原子事务；SDK不自动核销
`DurableAgent::NeedsRecovery`，也不能验证宿主是否真的先保存了自己的恢复记录。

`abandon_unknown(handle, host_note)` 是宿主明确放弃本地恢复，不等于远端取消、退款、
执行失败或授权重放；tombstone 仍禁止原 context 再次 POST。逻辑删除不提供物理安全擦除。

## 容量和范围

每个受保护 store 默认最多保留 64 个 session 名（可设 1–256）；reservation、unknown 和
tombstone 都占名额，不在异常后退款或回收旧身份。每 session 默认最多 64 个调用历史、
1 次显式 GET、256 个数据事件、256-byte event ID、1 MiB 请求、8 MiB journal。
原响应硬上限 2 MiB，所有值有静态硬上限。首次 POST 前为可能的完整结果及事件元数据
预留 journal 空间。ACK 释放结果正文空间，tombstone 不复用原调用身份。
容量耗尽要求宿主审核后选新逻辑 session/授权域与新 run；没有自动 eviction/reset。

不提供自动重连、未知 callback 恢复、自动 OAuth 交换/刷新、App 集成、服务器管理路由、
分布式工具 exactly-once 或自动将结果写回 RunStore。这些限制不影响旧单进程接口。

## 验证

`cargo test --locked --features mcp-persistence --test agent_mcp_persistent` 使用真实 HTTP/SSE、
SQLite 和独立测试进程，验证 effect 已 fsync 后 kill→新 owner GET；Observed 已 SQL 提交而
RunStore 尚未采用时 kill→无新 HTTP 读回；两个实际 successor 争同一 lease；强制 stale epoch
拒绝新 HTTP/状态写入；精确 ACK、tombstone、scope/输入绑定、callback/no-ID/404/错 session/
错 request/损坏结果/超量/重复 event ID 等负例。ignored helper 由父用例实际启动，不计成
独立默认 pass。显式 owned PostgreSQL 测试重复跨进程/fencing 与相同协议负例；其数据库
和容器须由调用宿主独占并清理，默认 ignored 不代表已通过。

本轮具体源码摘要、默认/owned PG 测试及独立代理审查范围见
[2026-11-28 验收元数据](acceptance/mcp-durable-20261128.json)。这只证明上述受限 SDK 合同，
不是通用 MCP 互操作、外部安全审计或所有服务都能 exactly-once 恢复的认证。
