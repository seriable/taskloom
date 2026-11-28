# 显式语义记忆与槽位审核

`runtime::memory` 新增可选的 SDK 语义召回。默认记忆路径仍使用原有
lexical recall；构造 `SemanticMemoryPolicy` 并显式调用
`MemoryManager::semantic_index` 才会把当前 owner **已审核 Active** 的
`key/value` 交给宿主提供的 `EmbeddingModel`。候选、已拒绝、已替代记录、
原始对话、来源 quote 和其他 owner 的记录均不进入该请求。

此接口不自动发现服务或读取凭据。宿主提供的 embedding transport 必须
负责请求前持久化预算、超时和未知用量；embedding 是独立服务费用，不能
从 generation 的 `RunBudget` 推导为已计费或免费。模型 fixture 的结果
只验证控制流程，不证明真实语义效果。

```rust,ignore
let policy = SemanticMemoryPolicy {
    max_records: 128,
    max_input_bytes: 128 * 1024,
    min_score: 0.75, // 宿主校准，不是所有模型通用阈值
    recall: MemoryRecallPolicy::default(),
};
let index = manager.semantic_index(&authenticated_access, embedding, policy).await?;
let result = manager.semantic_recall(
    &authenticated_access, &index, embedding, "我希望用哪种语言回答？"
).await?;
```

索引只存在于内存，没有 Deserialize 或自动磁盘缓存入口。索引绑定整个
已校验记忆快照、scope、revision、embedding identity 及全部 policy。
owner、模型、审核、编辑或遗忘改变时，旧索引在付费 query 前被拒绝；
构建每个后续 batch 前和模型返回后再次检查。query 期间的遗忘会丢弃整个
结果；已经发出的外部请求及费用不能撤回。曾读取的数据副本和调用者持有的
旧向量也不能由此 API 远程擦除。

最多 256 条、总输入 128 KiB，每 batch 最多 32 条，不自动重试。返回向量
必须符合 model identity、数量、维度、有限非零值约束。查询 cosine 得分
单列在 `scores`，`matched_terms` 仍只表示 lexical 匹配，不伪造匹配词。
`include_preferences` 显式开启时，已审核 preference 可以不经过相似度门槛；
这与原来的偏好召回语义一致。整个结果（含得分）受 `max_bytes` 约束。

`MemoryOntology` 是宿主提供的有界别名映射，例如
`reply_language -> language`，不允许链或环。`curation_plan` 只检查当前
owner 的 Active/Candidate，列出相同 canonical slot 的记录和精确值是否
相等。不同值表示需要审核的冲突；相同值也只是去重建议。接口不会合并、
接受、遗忘、改写 record identity 或授予任何权限。现有审核 CAS 和冲突替换
仍需显式操作；跨别名的合并也需宿主决定，不能把模型相似度当作同一事实。

当前交付为 SDK 接口，本轮没有在私人会话 UI 自动启用向外发送记忆。
调用者须在操作时进行认证和授权；`MemoryAccess` 本身不是身份验证服务。
不宣称提供分布式记忆向量库、持久化增量语义索引或全自动 ontology 学习。

本地控制测试：

```powershell
cargo test --locked --features runtime --lib memory::semantic
```

覆盖前置容量拒绝、只发送审核记录、跨 owner 不付费、过期索引不付费、
并发遗忘丢弃结果、非法向量拒绝、无 lexical 命中时的独立 semantic 分数、
只读 ontology 建议和别名环拒绝。
