# Qdrant 向量库 SDK 连接器

`qdrant` 是默认关闭的独立 feature，依赖 Rust runtime 与 HTTP client，
不引入 App/UI，也不改变默认 `features = []`。它提供真实 REST
`QdrantClient`、显式发布 journal 与 `QdrantRetriever`；不仅是后端 trait。

除 SDK 和可运行示例外，App 已提供可选 Hybrid 配置和本地主机显式发布
CLI，见 [App Qdrant 集成](app-qdrant.md)。UI 发布入口、外部索引增量同步、
SQL publish journal 和托管向量服务尚未接入。

## 固定宿主目标与本地来源

宿主通过 `QdrantConfig` 固定 endpoint、collection、namespace、凭证
环境变量名、超时、请求/响应字节上限和每批点数。静态 `validate()` 不读
凭证；显式构造 client 时才解析 named credential。endpoint 禁止 URL
凭据、query、fragment 和非根路径；只允许 HTTPS 或明确 loopback HTTP。
client 禁止 redirect、代理和协议自动重试。请求/model 不能改目标或取得
写入/建库权限。

宿主先用已有 `EmbeddingIndex` 与完整 `CorpusSnapshot` 建立对应关系。
连接器校验所有 chunk、维数、向量、模型身份和完整 snapshot 指纹，绑定
target/config 与 index 内容；不是只绑定一个可变 collection 名。
Qdrant 点使用从绑定与 chunk ID 派生的确定性 UUID。payload 包含精确
namespace、corpus/model/index 绑定、chunk ID 和格式版本，不存完整来源文本。

查询前再次核对本地 snapshot/index/model；不匹配时不调用 embedding。
同时以维数和完整固定 filter 计算保守请求上界，必然不满足大小配置时在
付费 embedding 前拒绝。连接器先检查远端现有 collection 为相同维数的
未命名 Cosine vector，再请求 query embedding。每次 query 固定带完整
scope/binding filter，`top_k` 最多 20。

收到响应后，逐个验证所有点的 UUID↔chunk ID、唯一性、有限 cosine 分数
和完整 payload；低于分数门槛的点也必须先通过验证。未知/重复点、额外
payload、错误 scope、模型或 snapshot 混入都会使整个查询失败。返回
`ScoredChunk` 后由标准 `search()` 从原本地快照定位文本、source 和 UTF-8
范围；远端返回的文本不能替换证据。它能防止外部结果注入其他来源，但
不能证明远端没有删点、遗漏结果或编造排名，因此不把它当作召回完整性
或事实正确性证明。

## 显式发布与未知写入状态

`QdrantClient::publish(snapshot, index, journal)` 只上传宿主已经构建好的
向量，不自动生成 embeddings、不创建/删除 collection，也不从 query
偷偷调用 upsert。collection 必须由宿主另行预建，且通过维数/距离验证。
宿主应先决定何人可以执行发布，再调用 SDK；当前没有开放远程写入 API。

所有批次在首次写入前检查完整请求大小。每个实际 HTTP upsert 之前，
`FileQdrantPublishJournal` 都先原子写入并同步 `pending`、批次 ordinal、
请求 SHA-256 与点数。只有 `wait=true` 且服务返回 `completed` 与 operation
ID 后，该批次才标记 acknowledged。全部批次持久化确认后才能取得
`QdrantPublishReceipt`；该类型没有公共构造或 Deserialize。进程重启后
可用 `completed_receipt()` 校验已完成的宿主本地 journal。

`pending` 包括可能已写入但丢失回复的情况，必须按 unknown 处理。超时、
中断、不完整 ack、取消或确认写入 journal 失败都不会授予 complete receipt，
不能用该 journal 开启 query。已有 journal 禁止 `publish` 隐式重启/重试，
失败状态保留以便人工检查。跨进程文件锁保护 journal；此处是宿主私有文件，
不能接收客户端上传的 journal。

当前没有精确 batch 自动恢复 API。Qdrant upsert 相同 point ID/内容具有
自身的覆盖幂等性；宿主核查后可以另开一次明确发布，使用同一冻结 index
及目标，但应保留原未知 journal。这是 Qdrant 的原生语义，不能推导其他
工具或任意外部写入也可以重放。新的发布也不删除旧 snapshot 的点；
查询依靠精确 immutable binding 过滤历史点，回收由宿主单独管理。

## 费用边界

发布复用已经建好的向量，连接器自身不创建模型 Provider。外部
`EmbeddingModel` 必须由宿主提供持久化的付费预留/结算/unknown 保留，
不能把 SDK trait 当成免费服务。App 接入复用既有 metered embedding
factory；自定义宿主也不能自行构造未计费 HTTP embedding client。

Qdrant 存储、查询、网络和基础设施费用不属于 generation `RunBudget`，
也不在 embedding 费用账本里。SDK 只提供有限批次、无重试的显式请求；
实际托管服务的费用政策和权限由宿主配置。这一版示例只用标注的离线
向量夹具，无外部 generation/embedding 模型调用。

## 本地真实 Qdrant 验证

镜像固定为 upstream `qdrant/qdrant` **v1.20.2** 对应的镜像转发版本。
官方 GitHub release 已读取核对；本机直连 Docker Hub 超时，实际下载经过
第三方 DaoCloud，不能声称镜像由直连官方 registry 获取。manifest digest、
Docker image ID、实际运行版本/commit 都写入本地 public provenance。

```powershell
docker pull docker.m.daocloud.io/qdrant/qdrant@sha256:d277592721d2e3b73f9b797b53cf05e6a5bb2134f6d13fdbda4ca51782afbcac
cargo build --locked --features qdrant --example agent_qdrant
cargo test --locked --features qdrant --test agent_qdrant
python examples/database/qdrant_fixture.py create target/qdrant-owned-01
python examples/database/qdrant_fixture.py accept target/qdrant-owned-01 `
  --out target/qdrant-accept-01 --example target/debug/examples/agent_qdrant.exe
python examples/database/qdrant_fixture.py remove target/qdrant-owned-01
```

Linux 替换为无 `.exe` 路径。输出目录使用新名称。helper 不隐式 pull、
不改 daemon，只启动自建带 nonce label 的 loopback 容器，关闭遥测，
凭证经环境传递；清理必须同时匹配确切 container ID、label 和 image ID，
只删除该容器及其匿名数据卷。失败也应执行最后的清理命令。

`accept` 复制二进制后运行完整示例，真实发布主 scope 两个点、另一个 scope
一个点，检查实际查询、本地来源、stale 无 embedding、完成 journal，以及
真实服务的 total=3、分 scope=2/1。`report.json` 记录二进制、源码和日志
SHA-256。测试的“写成功但丢失 ack”、恶意结果与大小边界通过真实 loopback
HTTP 协议夹具单独触发；不将其声称为真实 Qdrant 节点故障注入。

输出包含宿主配置、运行记录和私有 API key，仅保留在 Git 忽略的 `target/`，
不能把整个 fixture 目录发布到开源仓库。这些验证证明的是单机 REST 与
来源/状态契约，不代表语义效果、生产容量、多节点故障、云账号计费、
TLS 运维或生产 SLO 已完成。
