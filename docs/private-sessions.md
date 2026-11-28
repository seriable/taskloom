# 用户私有会话与共享项目预算

私有执行需要独立、明确的宿主授权。`private_memory` 只开启已认证用户的私有
记忆 CRUD，不会启动模型、自动提取或把共享会话变成私有来源。
`ServiceOptions.private_execution` 或正式 service host 的同名配置另行启用
私有会话控制器；默认关闭。

```json
{
  "private_memory": {"curate":true,"recall":{"max_items":8,"max_bytes":16384,"include_preferences":true}},
  "private_execution": {"max_active_runs":2,"max_retained_jobs":256}
}
```

这是宿主配置片段，不能通过用户请求提交。需要实际 SQL Application、明确模型
价格、每运行预算以及已经初始化并启用的项目 generation quota。首次创建作用域
与项目 quota 检查在同一事务内；未初始化/禁用的项目不能创建新的私有作用域。

## 不同的数据范围

共享项目仍使用原有 `(tenant,project)` SQL namespace；私有运行、会话和工作流
使用不可由旧构造器创建的 canonical `(tenant,project,authenticated_user)` namespace。
作用域不靠 ID 前缀过滤，也不改变公开共享记录的 ID。私有历史不会出现在共享项目
的 run/session/workflow 列表、队列、诊断或普通用户的另一个 namespace。

private scope 固定记录原始项目 quota 映射。恢复时不允许更换项目或缺失绑定后
自动采纳旧数据。私有 budget 保存仍先锁运行 ownership，再锁共享项目 quota，
在同一事务中更新 private journal 和 project accounting。
私有 accounting 使用独立 `quota_private_run` family；即使共享调用者选择同一个
hash 作为 run ID，也不会覆盖私有账目。预算记录保持原格式，旧共享记录不被迁移
或重写；quota 初次导入遇到残留 private accounting 会拒绝不完整的回填。

私有并发上限是这个控制器的独立宿主限制，不能冒称共享队列的
`max_running/max_open_jobs` 同时覆盖私有任务。私有和共享模型金额、在途及未知
尝试则真正共享项目 quota；零计价的未知请求仍占用项目尝试槽，不因暂停、关闭
HTTP、禁用或重新启用策略而退款。现有 private scope 可在禁用 quota 后只读或
保存已有结算；新的私有模型 reservation 每次事务重新检查 policy 必须启用。

## 当前执行契约

私有会话当前明确使用 `no-tools-v1`：不会继承共享 Application 注册的工具、技能、
MCP、sandbox 或写权限。它不支持私有写工具审批、工具效果 reconciliation 或私有
工作流 API；这些路由明确拒绝，而不是把共享审批记录套入私有任务。

模型、输出上限、context、价格及 final-output policy 仍来自宿主配置。标签绑定
宿主及真实所有者，既有运行不能在换配置后静默恢复。没有隐式模型、外部服务或
自动记忆提取。用户更正和记忆都保持 User/data 身份，不授予工具或系统权限。

每个逻辑模型步骤重新检查真实 SQL 身份、成员关系和撤销。已经被接受的步骤仍
可能按 Provider 契约执行内部重试/降级；不承诺每个上游 HTTP 尝试的原子撤销，
也不能撤回已经发送的请求。取消、超时和进程退出必须保留原有未知用量。

## API

所有路径位于已注册项目的
`/tenants/T/projects/P/api`，要求相应有效 bearer。所有者和 audit actor 均从
已认证上下文取得；请求不能选择 owner、tenant、project、工具、模型或权限。
Reader 可以读取自己的内容；Runner/Operator 可以提交自己的任务。私有记忆提取
还要求宿主 `curate:true`，产生候选后仍需要显式审核。

| 方法与相对路径 | 输入/行为 |
| --- | --- |
| `GET /private/sessions` | 当前用户的私有会话列表 |
| `POST /private/sessions` | `{id}`，用宿主 system 创建会话 |
| `GET /private/sessions/ID` | 本人完整历史、active binding 和关闭的 turn |
| `POST /private/sessions/ID/turns` | `{turn,prompt}`，显式新轮；返回 202 后检查 status/会话 |
| `POST /private/sessions/ID/memory-turns` | 相同输入，冻结本人已审核记忆的有界召回 |
| `POST /private/sessions/ID/resume` | `{turn}`，仅恢复本人原冻结轮次；不替换 prompt/config |
| `GET /private/sessions/ID/status` | 当前进程的有界 job 状态；无记录不代表旧运行成功 |
| `POST /private/sessions/ID/cancel` | `{}`，请求取消当前进程的本人任务，随后检查实际结果 |
| `POST /private/sessions/ID/acknowledge-failure` | `{turn,note}`，确认已终止失败并关闭原轮次；原未知预算不清除 |
| `GET /private/sessions/ID/diagnostics` | 仅本人的来源绑定 Trace 投影，不自动外发 |
| `GET /private/runs`、`GET /private/runs/ID` | 本人私有运行和保存状态 |
| `GET /private/runs/ID/diagnostics` | 本人运行诊断 |
| `POST /private/memory/extract` | `{expected_revision,session_id,turn_id,request_id}`，只从成功关闭的本人私有 User 轮次提取 |
| `GET /private/memory/extractions/REQUEST_ID` | 本次提取的进程 job 状态及实际 run ID |

记忆审核、编辑、遗忘仍走该用户的 `/memory/...`，它不访问任何共享或他人记忆。
共享 `/memory/extract` 与共享 session memory-turns 不会因此获得授权。不能从一个
共享 User turn 自动断言用户私有偏好，也不把模型/工具/召回记忆当成新用户来源。

后台 job map 按 owner/resource 分域，并以 `max_active_runs` 限流；最多保存
`max_retained_jobs` 条进程状态，只淘汰已结束条目，不删除 durable 历史。
重启后以 SQL session/run 为准，进程 job 表不构成状态存储。服务退出先关闭
admission，取消并在 grace 内等待私有 job；超时必须保留原 journal 并显式核对。

## 验收与限制

`tests/private_session_stores.rs` 在真实 SQLite/PostgreSQL 验证两 owner 与共享
同 ID 隔离、多 pool 原子 quota 竞争、零价未知占槽、disabled settlement、不同
accounting family、冻结映射和不完整 backfill 拒绝。
`tests/app_private_sessions.rs` 使用真实 HTTP/SQL 和明确 fixture 模型，覆盖私有
历史/来源提取/审核/召回、无工具契约、终态 reentry、并发、取消、共享数据不回流，
以及第一次输出后撤销 token 阻止下一逻辑 repair 步骤。fixture 不是原生模型质量
或费用/延迟基准；不能据此声称模型事实正确、所有攻击都被阻挡或已验证生产规模。

生产级私有写工具、多机/多副本私有任务调度、统一私有与共享运行并发 cap、私有
工作流产品和完整 Kubernetes 真实部署仍需单独实现/验收，不能从本地控制器或
SQL 事务推导出这些能力已经完成。
