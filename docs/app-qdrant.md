# App 的显式 Qdrant 索引发布与查询

App 在 `app,qdrant` feature 下支持 Qdrant Hybrid 检索。默认配置仍是本地
lexical；`retrieval.qdrant = None` 时序列化省略该字段，保持旧配置身份。
SQL 使用时还需 `database` feature。底层连接器契约见
[Qdrant SDK](qdrant-connector.md)。

## 配置与费用

在已有完整 App 配置的 `retrieval` 中增加固定宿主目标：

```json
{
  "qdrant": {
    "endpoint": "http://127.0.0.1:6333/",
    "collection": "taskloom_knowledge",
    "namespace": "team-main",
    "api_key_env": "TASKLOOM_QDRANT_API_KEY",
    "timeout_ms": 5000,
    "max_response_bytes": 1048576,
    "max_request_bytes": 1048576,
    "batch_points": 64
  }
}
```

这是 `retrieval` 的局部配置，不是完整 App 文件。还必须显式配置
`retrieval.embedding` 的模型、Provider、密钥环境变量名、价格、单次预留和
总费用上限。App 复用该持久化 embedding 账本完成索引构建与 query，
missing usage、超时和失败保留 reservation/unknown，不静默退款或重试。
Qdrant 存储、网络和基础设施费用独立于 embedding 与 generation 配额。

App 会将声明的 namespace 与 `AppStores.identity()` 再绑定为 effective
namespace。因此即使两个 SQL 项目填相同 namespace，它们也不会共享同一
远端点空间。该绑定还参与 deterministic point ID 与完整发布 receipt；
请求、模型或技能不能另选 endpoint、collection、namespace 或凭证。
规划视图和技能宿主只声明凭证**名字**，不会把凭证值提供给模型。

## 构建、发布、启动

先由宿主在 Qdrant 建好相同维数的 unnamed Cosine collection；App 不自动
建库/删库。以下命令使用本机 `taskloom`，不要把本机发布命令当作 HTTP API：

```text
# retrieval.mode 先保持 lexical，显式导入并检查。
taskloom --config taskloom.json import policy Policy policy.md
taskloom --config taskloom.json index-status
taskloom --config taskloom.json index-build CORPUS_REV CONFIGURATION_BINDING
taskloom --config taskloom.json index-status
taskloom --config taskloom.json index-publish-qdrant CORPUS_REV CONFIGURATION_BINDING INDEX_BINDING

# 再将 retrieval.mode 改为 hybrid，重新启动进程。
taskloom --config taskloom.json search "需要查询的内容"
```

`CORPUS_REV`、`CONFIGURATION_BINDING`、`INDEX_BINDING` 读取最新
`index-status`，不得拿旧回执套新源或新服务配置。`index-build` 只走现有
metered embedding 构建本地/SQL index，不上传 Qdrant；发布只重用已有
向量，不再次调用 embedding。publish CLI 是显式宿主操作，需要完整三重
绑定。没有 `/retrieval/qdrant/publish` 远程路由；查询、服务启动、
`index-status` 和旧 `/retrieval/index` 构建 API 均不会自动发布。

发布 journal 固定派生为项目 `state_dir/retrieval/qdrant/<精确 target+index
digest>`。调用方不能传新 journal 名，也不能通过重跑同一命令绕过 unknown。
已有同 binding journal 无论完成或未完成，都要求先检查，不自动再次 upsert。
每批次派发前先持久化 pending，再检查当前完整 corpus/index；SQL 模式同时
核对实际 index lease/epoch。丢失 owner 或源变化会阻止后续批次。已经发出的
外部请求不属于数据库事务，必须保留已发生/可能发生的效果。

失败或丢失 ack 保留 pending/unknown，不能取得 query receipt。即使只是
人为把 `complete` 改成 true，冷加载也会重算并核对实际 target/index 与**所有**
批次的 ordinal、point count、request SHA-256、ack 状态，不只看一个布尔值。
这些文件是宿主私有持久化，不接受来自远程请求的 receipt 导入。

## 查询与冷检查

lexical 模式只创建原有本地 retriever，不构造 Qdrant client，也不读取
Qdrant/embedding 凭证。`config-check` 和 `index-status` 不读这些 key、
不请求模型/向量服务；后者返回 `qdrant.ready/state/journal/error`，区分
未发布、完整完成以及不匹配/未完成。`ready` 是对当前本地 snapshot/model
与完整 plan 的校验结果，不是远端 HA 或数据完整性保证。

Hybrid 必须先通过当前 local index 与完整已完成 receipt 校验，才能构造
metered query Provider。每次查询又会检查完整源快照/模型、保守请求大小、
collection 维数与 Cosine 配置，然后才调用 query embedding；错误不会回退
到 lexical 冒充成功。所有远端点都校验 scope/binding/ID/score 后，引用与
正文仍从原本地快照定位。修改源后旧 index 会在付费 query 前拒绝，需要
再次显式构建、发布与重启。

原有 Hybrid fusion、semantic cutoff/gate、exact identifiers、local/HTTP
reranker 继续应用。当前 Qdrant Hybrid 尚不支持 `query_prefix` 和
`semantic_min_lexical_terms`；配置这些组合会静态拒绝，不能悄悄忽略规则。
本地 VectorRetriever 的相关功能保留。

SQL index 位于数据库，publish journal 仍位于宿主项目 state_dir。多副本
部署需要宿主显式分发/挂载相同完整 receipt 文件；本轮不声明多副本自动
同步、SQL publish journal 或远端增量索引回收已经完成。

## 验收工具

`tests/app_qdrant.rs` 通过真实 HTTP 与 file/SQLite/owned PostgreSQL 检查：
冷检查、默认关闭、lexical 不访问外部服务、跨项目 namespace、完整 receipt
篡改拒绝、固定 unknown journal、metered query/missing usage/timeout、
低请求上限无付费派发、exact identifiers/rerank、源变更或 SQL owner/epoch
丢失后不派发下一批。这里的向量服务是明确标注的协议夹具。

实际 Qdrant CLI 工具使用单独的 owned fixtures：

```powershell
cargo build --locked --all-features --bin taskloom
python examples/database/qdrant_fixture.py create target/app-qdrant-owned-01
python examples/database/postgres_fixture.py create target/app-qdrant-pg-01
python examples/operations/qdrant_app.py target/app-qdrant-run-01 `
  --taskloom target/debug/taskloom.exe `
  --qdrant-fixture target/app-qdrant-owned-01 `
  --postgres-fixture target/app-qdrant-pg-01
python examples/database/qdrant_fixture.py remove target/app-qdrant-owned-01
python examples/database/postgres_fixture.py remove target/app-qdrant-pg-01
```

helper 只使用已有固定 digest 镜像，镜像准备与第三方来源见 SDK 文档。
输出目录须用新名称，成功/失败后都应清理确切 owned fixtures。实际工具
分别执行 SQLite/PostgreSQL CLI 构建、发布、全新进程 Hybrid 查询、冷检查、
missing usage 与 stale 拒绝，还通过独占 loopback proxy 将真实 Qdrant
已 completed 的 upsert ack 丢弃，验证原 journal 保留 unknown 且不重试。
这是受控网关回复丢失，不是真实多机分区或生产 SLO 验收。

回执含源码/二进制/日志 SHA-256、真实服务版本与 image 来源、持久化预算
预留观察和每项结果。模型响应是本机 HTTP 协议夹具，不能将本次服务集成
测试当作真实 embedding 语义效果或云计费验证。凭证、数据库和完整运行
记录仅保存在 Git 忽略的 `target/`，不作为开源源代码发布。
