# 显式增量 embedding 索引

`runtime` SDK 新增 `EmbeddingIndex::update_incremental`。宿主必须同时持有：
旧的完整 `EmbeddingIndex`、它绑定的原始 `CorpusSnapshot`、经过授权的新完整快照，
以及实际使用的同一个 `EmbeddingModel` 合同。只有满足这些条件才能复用向量，不能
从一个旧 fingerprint 或新语料猜出旧索引的原始来源。

```rust,no_run
# #[cfg(feature = "runtime")]
# async fn example(
#     old_index: &taskloom::runtime::rag::EmbeddingIndex,
#     old_snapshot: &taskloom::runtime::rag::CorpusSnapshot,
#     new_snapshot: &taskloom::runtime::rag::CorpusSnapshot,
#     embedding: &dyn taskloom::runtime::rag::EmbeddingModel,
# ) -> Result<(), taskloom::runtime::RuntimeError> {
let update = old_index
    .update_incremental(old_snapshot, new_snapshot, embedding)
    .await?;
match update.index {
    Some(new_index) => {
        // Host explicitly persists the index together with its exact snapshot.
        // Do not replace a newer concurrently published corpus/index revision.
        let _ = new_index;
    }
    None => {
        // All chunks were deleted. Host explicitly clears its active index.
        // This call does not delete a file, collection, source or corpus.
        assert!(update.receipt.index_absent);
    }
}
# Ok(()) }
```

## 支付前校验与向量复用

在第一次 embedding 调用前：

1. 完整校验旧、新语料的 document / chunk identity、真实 UTF-8 范围和 source 绑定。
2. 按实际 `model.identity()` 校验旧索引的版本、body-only / title context 模式、
   完整旧 snapshot fingerprint、维度、向量数、全部有限/非零向量与键集合。
3. 新 revision 必须严格大于旧 revision；唯一例外是同 revision 且完整 snapshot
   fingerprint 完全相同的 no-op。倒退或同 revision 不同内容会零调用拒绝。
   可以跨过多个明确的 corpus revision，不要求每个中间快照都保留；这不能证明
   两个不同存储库拥有共同历史，宿主必须自己选择经过授权的同一来源 lineage。
4. 使用已知旧维度校验最终索引的 1–10000 chunks、维度 1–4096 与总计
   4000000 个 vector values 上限；过大的新索引在第一笔服务调用前拒绝。

只有新旧完整 Chunk 完全相同，并且 `document_text(chunk, document_context)`
完全相同，才复制旧向量。不能按文本相似、相同 body、标题、文档 ID、文件名或向量
分数猜测复用。source URI、标题、document ID、offset 或正文改变都会改变 chunk
identity；即使正文相同，另一个来源也需要重新 embedding。任意未影响该 chunk /
embedding input 的文档 metadata 变化仍会改变整个 snapshot binding，但不会为它
伪造一次新模型请求。

只把不满足复用条件的新 chunks 按新快照顺序、每批最多 32 条交给原 host model。
每批发出前、响应后与返回前检查实际 model identity；响应 identity、数量、维度、
有限值和非零范数必须保持旧 vector space。不会自动更换模型、降级、重新嵌入全部
向量或为错误批次重试。旧索引和两个输入快照均不原地修改。

这些是结构与来源绑定检查，不是向量质量、模型服务诚信、签名或人工相关性认证。
索引本身是可序列化数据，宿主需保护存储完整性，不能信任客户端上传的“合法向量”。
模型身份仍需由 host 绑定真实 provider / model / version / endpoint / tenant，不能仅
用同名标签掩盖不同 vector space。真实相似度效果仍需独立评测。

## 删除、receipt 与成本

成功返回 `EmbeddingIndexUpdate { index, receipt }`。receipt 包含：

| 字段 | 含义 |
| --- | --- |
| `old_corpus_binding` / `new_corpus_binding` | 两个完整快照的 fingerprint |
| `old_index_binding` / `new_index_binding` | 完整旧、新索引 fingerprint；清空时新值为 None |
| `model_identity` / `document_context` | 本次固定模型与原 embedding 输入模式 |
| `reused` | 原样保留的旧向量数 |
| `embedded` | 本次成功生成并校验的新向量数 |
| `removed` | 没有复用的旧向量数，包含被修改来源替换掉的旧 chunks |
| `index_absent` | 新语料没有任何 chunks，需要宿主显式清除活动索引 |

`旧数量 = reused + removed`，`新数量 = reused + embedded`。只删一部分时，返回
的新索引没有被删向量；删光时返回 `index=None`，保留既有“不接受空 EmbeddingIndex”
格式规则。函数不会删除旧对象，旧索引也不能通过新空快照的 `validate`。宿主要明确
处理 None；保留旧文件然后继续使用旧语料不是一次成功的索引清除。

receipt 只有整个操作成功才生成，不是签名审计或持久化确认。它统计有效向量，不能
当作 HTTP 尝试数、token 用量或收费回执。一次 host model embed 可能已经发生
外部请求；失败、取消、identity / vector 校验失败后，没有部分索引或部分成功 receipt
可供自动续接，也不会退还/清除此前费用。原 model transport 必须在实际派发前完成
原有 durable reservation，并保留未知用量；embedding 不归 generation RunBudget
计费。想复用本次未提交的部分结果需要独立、可审计的持久化协议，本包没有实现。

## App 与 source 同步边界

没有更改旧 `EmbeddingIndex` JSON、版本 1 / 2、`document_context=false` 的默认
省略行为、完整 build 路径或既有 retriever 身份。新接口是单独显式 SDK 操作。

App 当前没有保存每个旧索引的完整旧语料快照，因此 `index-build` 仍是完整构建，
没有暗中切成增量，也没有新增 HTTP / CLI 的自动增量开关。宿主接入前还需要保存
与索引原子配对的旧 snapshot、持久化 CAS、进程崩溃与未知提交恢复、并发更新及
索引读者切换。单独把一个新索引写到磁盘不等于与 corpus 原子发布。

[数据库与对象来源快照](data-source-snapshots.md) 与此接口仍是独立的只读数据合同。
没有跨 SQL / 对象源 / FileCorpusStore / Qdrant 的共同 transaction、删除传播、cursor
或分布式同步。不能把这个向量复用方法说成生产级外部 source 增量同步。

## 本地验证

```powershell
cargo test --locked --features runtime --test agent_incremental_index --test agent_hybrid
```

实际 FileCorpusStore / SDK fixture 验证：同快照及只升 revision 的更新零调用；单源
中间一个 chunk 修改只 embedding 该新 chunk；删除和 clear-all 不派发；同正文但
不同来源/标题不混用；旧快照、revision、模型、context 和旧坏向量都支付前拒绝；
坏响应 / provider error / 运行中 model 变更不重试、不返回 partial index、不改旧索引；
70 个新增 chunk 以 32/32/6 分批，最终超容量在首次调用前拒绝。模型向量是显式控制
fixture，不是实际 embedding 效果或真实服务费用验收；原 hybrid 测试另作兼容回归。
