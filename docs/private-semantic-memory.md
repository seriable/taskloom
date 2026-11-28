# 显式私有语义记忆召回

多人 SQL 服务可以额外启用语义召回。默认仍是 lexical
`POST /api/memory/recall`，不上传记忆、不读取 embedding 凭证。
语义功能必须由宿主开启，并由用户单独调用
`POST /tenants/T/projects/P/api/memory/semantic-recall`。普通会话、
`memory-turn`、记忆读取和审核不会自动触发此操作。

## 宿主配置

正式 host 配置中的 `private_memory` 增加可选 `semantic`；省略或设为
`null` 即关闭。旧配置序列化时不增加该字段。

```json
{
  "private_memory": {
    "curate": true,
    "semantic": {
      "max_records": 64,
      "max_input_bytes": 65536,
      "min_score": 0.8,
      "recall": {
        "max_items": 8,
        "max_bytes": 16384,
        "include_preferences": false
      }
    }
  }
}
```

这是 host 文件的局部配置。注册的每个项目必须使用匹配的 SQL storage，
并在该项目 `AppConfig.retrieval.embedding` 中显式配置 embedding Provider、
模型、凭证环境变量、价格、每次费用预留、总费用上限、输入大小和超时。
沿用[检索配置](retrieval-relevance.md)的持久化计费实现；不另建未计费 HTTP client。
`service-check`、App 配置验证和服务路由注册只校验配置，不读取 embedding
密钥、不连接模型服务。只有显式请求 handler 构建 Provider 时才解析凭证。

`max_records` 为 1–256，`max_input_bytes` 为 1–128 KiB；后者计算本次
全部已审核记录的 `key + 换行 + value` 总字节。超界拒绝整个操作，不截断
候选后悄悄上传。实际 Provider 还有自己的每请求输入限制。`min_score`
是 cosine 分数门槛，范围 0–1；若显式允许 preferences，则已审核偏好可
按该策略参与结果。`recall` 同时约束返回记录数和完整响应字节。

## 调用与隔离

```http
POST /tenants/T/projects/P/api/memory/semantic-recall
Authorization: Bearer <user bearer>
Content-Type: application/json

{"text":"我之前确认过的相关偏好是什么？"}
```

请求只接受 `text`，须为 1–4096 字节的有效记忆文本；不接受 owner、scope、
tenant、project、Provider、向量或凭证字段，也不接受 query 参数。
该操作可能产生外部费用，要求项目 `Submit` 权限；Reader 可继续使用原有
lexical 只读召回，但不能发起付费 semantic 召回。

`GET /api/identity` 的非敏感 `semantic_memory_enabled` 布尔值供界面显示
宿主是否开启此功能；它不授予 Submit 权限，也不能替代 endpoint 的实时授权。

scope 的 tenant/project 来自宿主注册，owner 来自已认证服务用户。
Operator 的项目管理权限也不能读取其他 owner 的记忆。只有该 owner
已审核的 `Active` 记录会进入索引；Candidate、Rejected、Superseded 和
已遗忘记录均不上传。查询只发送 query 文本，索引批次只发送审核记录的
key/value，不发送来源历史、完整会话或权限信息。

每次显式请求重新建立临时 index，再发起 query embedding；每批最多
32 条记录。向量不接受反序列化，不持久缓存，也不写回共享 session/run。
响应是 `SemanticMemoryRecall`：`memory` 中含 owner scope、revision、query
和带来源的 hits，另有 `scores` 与内部 `index_binding`；响应始终
`Cache-Control: no-store`。该结果是待使用的记忆数据，不能授予工具权限。

## 费用、撤销与并发变更

整个操作沿用同一项目 `retrieval.embedding` 的独立持久化费用账本，
因此不同 owner 的显式召回以及该项目知识检索共享 embedding 支出上限。
它与 generation RunBudget/项目生成配额分账，不能把 embedding 费用
当作已经包含在模型生成上限内。每次 HTTP embedding 之前，完整请求预留
先写入 SQL；成功且有 usage 时按配置价格结算。缺失 usage、超时、取消、
网络/协议失败保留 reservation/unknown，不隐式退款、重试或降级。

每个逻辑 embedding batch/query 前后都会重新读取 SQL 授权，核对原用户、
token 撤销/到期、enabled 状态和当前项目 Submit 权限；OIDC actor 同时受
已验证的到期时间约束。付费调用期间丢失权限会丢弃结果，保留已产生的费用。
模型调用前的授权检查不能与外部服务请求组成原子事务，不能用它承诺撤销
瞬间取消已经发送的请求。

SDK 在每批次和 query 前后核对记忆快照/模型身份；handler 输出前再核对
完整 owner 快照及当前权限。并发 edit、review 或 forget 使本轮旧结果无效。
query 文本及最小响应 envelope 在任何索引付费请求之前预检，避免必然超出
输出上限的输入仍先产生索引费用。发生冲突需用户检查后显式重试；重试会
重新上传当前审核记录并可能再次计费。

## 已验证范围

`tests/app_semantic_memory.rs` 使用真实 loopback HTTP、SQLite 和单独运行
的 owned PostgreSQL 控制：跨 owner/项目、默认关闭、Reader 拒绝、无密钥
冷检查、候选/拒绝记录不上传、33 条审核记录的多 batch、持久化费用预留、
显式重建、batch/query 期间撤销、真实 token 到期、query 期间忘记，以及
missing usage 保留费用。

这些向量来自协议夹具，证明的是权限、计费与状态边界；不代表真实 embedding
模型的语义效果已达标，也不代表真实云账号、多副本服务或生产容量已验收。
