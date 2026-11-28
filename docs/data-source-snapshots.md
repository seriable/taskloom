# 数据库与对象存储的显式只读快照

这两个 SDK 入口只读取宿主选择的数据，返回惰性的 Document 或 bytes snapshot。
没有自动注册模型工具、HTTP 任意 SQL / URL / 服务器文件入口、后台扫描、自动删除、
corpus 替换或付费 embedding。导入仍由宿主检查来源、权限、容量及实际 corpus revision，
再显式调用现有 `ingest(..., Some(expected_revision))`。

## SQL：固定表、列与 namespace

可选 `database` feature 提供 `SqlDocumentSource`、`SqlSourceConfig`、
`SqlSourceLimits` 与 `SqlDocumentSnapshot`。这是外部知识来源连接器，不调用
Taskloom runtime migration，也不把源表改成运行时数据库。

```rust,no_run
# #[cfg(feature = "database")]
# async fn example() -> Result<(), taskloom::runtime::RuntimeError> {
use taskloom::runtime::rag::{SqlDocumentSource, SqlSourceConfig, SqlSourceLimits};
let policy = SqlSourceConfig {
    source_namespace: "reviewed-policies".into(),
    source_version: "v1".into(),
    table: "knowledge_source".into(),
    namespace_column: "project_key".into(),
    namespace_value: "authorized-project".into(),
    id_column: "document_id".into(),
    title_column: "title".into(),
    text_column: "body".into(),
    limits: SqlSourceLimits::default(),
};
let source = SqlDocumentSource::sqlite("reviewed-source.sqlite", policy).await?;
let snapshot = source.snapshot().await?;
// Review snapshot.documents, then explicitly apply corpus CAS operations.
source.close().await;
# let _ = snapshot;
# Ok(()) }
```

`postgres(secret_url, policy)` 使用宿主给出的 PostgreSQL URL。连接对象没有
Debug / Serialize，不保存原始 URL；错误不包含连接字符串、SQL 参数、服务端错误正文
或源行。连接禁用 SQL statement logging。URL / 密码环境变量值仍是宿主密钥，不能
放进模型、工具参数、公开 source ID、日志、文档或回执。公开 namespace/version 必须
由宿主单独选择，数据库、账号或授权范围改变时应显式改变来源身份。

表和四个互不相同的列名必须为 1–63 字节 ASCII 标识符：首字符字母或下划线，
其余只能字母、数字或下划线。不接受 SQL、表达式、schema 片段、JOIN、WHERE、
排序表达式或调用者 query。内部 SQL 只有固定投影 / 精确 namespace predicate /
稳定 ID 排序 / 有界 LIMIT 模板；只有通过校验并 quote 的标识符进入模板，namespace
值和 row limit 始终是 bind 参数。namespace 按 UTF-8 字节比较，不能被 SQLite
NOCASE 或 PostgreSQL 列 collation 放宽成另一个 namespace。

SQLite 只打开已经存在的 regular file，使用 `mode=ro` 和连接 `query_only=ON`，
不创建源文件。只允许 `main` base table，使用 `pragma_table_list` 拒绝 view、virtual /
shadow table，使用 `table_xinfo` 拒绝所选 generated/hidden 列。需要 SQLite 3.37+
的标准 table-list 功能。读取行的 id/title/body 必须实际为 TEXT，不自动把 BLOB 或数值
当作知识文本。文件路径与本机读取权限仍由宿主控制，canonicalize 不构成文件沙箱。

PostgreSQL 只允许 `public` 中的普通 base table（不含 view、foreign / partitioned
table），所选列类型为 TEXT / VARCHAR 且不是 generated column。连接固定
`search_path=pg_catalog`、默认 read-only，读取使用显式
`BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY`；函数调用为固定 PostgreSQL
内置函数。宿主仍负责源数据库、表定义、RLS、数据库账号和服务器管理；不把不可信
数据库服务器当作解析沙箱，也不声称这个 SDK 在服务器之外认证了最终用户。

## SQL 上限与一致性

默认每次最多 256 行、每个 body 1 MiB、完整序列化 Documents 合计 16 MiB，
5 秒总等待时限。硬上限为 4096 行、每 body 8 MiB、Documents 合计 64 MiB，
60 秒时限。ID 最大 512 字节、title 最大 4096 字节，空白 ID/title/body、重复 ID、
NULL、非法 UTF-8、错误类型或任何超限都会拒绝整个 snapshot。

先在同一个 read transaction 中查询 `max_rows + 1` 行的字节长度与类型。
多出的行触发错误，绝不静默截掉。长度与总原始 bytes 检查通过后，才读取有界
id/title/body 前缀；它们在同一事务快照中不会被并发编辑换成另一批数据。最终再按
实际序列化 Document 大小检查合计上限。SQLite 使用 deferred read transaction，
PostgreSQL 使用上述 repeatable-read read-only transaction；成功前不返回部分结果。
PostgreSQL 长度预检统一计算 `convert_to(column,'UTF8')` 的字节数，与正文提取完全
相同；不能用 LATIN1 等 server encoding 的原始长度冒充 UTF-8 输出大小。

PostgreSQL 还设置与等待时限一致的 server statement/lock timeout。SQLite busy
timeout 有限，但 Rust Future 的超时不能证明 SQLite worker 的计算已经立即结束；
大表筛选和排序可能扫描大量源行，数据库索引、容量与 CPU 隔离由宿主另行部署。
这里的返回行数/内容上限不是数据库扫描行数或跨库执行成本上限。

返回值包含公开 namespace/version、固定 policy SHA-256、整个有序行集的 snapshot
SHA-256、Documents。policy 和 snapshot hash 使用框架 canonical JSON fingerprint；
`text_value_sha256` 也是 JSON string value 的 fingerprint，不能与原始 UTF-8 文件
SHA-256 混用。Document ID 绑定 policy 和源行 ID，内容改变时仍能显式替换同一个
文档；source 同时绑定整个 snapshot，所以其他行变化也改变该批次的引用来源身份。
源 URL、数据库密码和原始行 ID 不进入 source/metadata。公开 hash 不提供加密或
防止小字典枚举，不能用它替代 source ACL。

没有匹配行时返回明确的空 Documents 和空集 hash。空结果不代表授权删除 corpus
旧数据；本接口不会做任何同步写入。多个 Document 的逐条 `ingest` 也不是一个原子
批次；想做全量 / 增量镜像仍需独立的稳定 cursor、revision、删除标记、事务性应用及
未知提交恢复协议，当前未实现。

## 对象存储：宿主固定的预签名 GET

可选 `research` feature 提供 `PresignedObjectSource`、`PresignedObjectConfig` 和
`ObjectSourceSnapshot`。当前支持宿主明确交接的 S3-compatible 预签名 GET URL，
也适用于遵守同样完整对象 HTTP 合同的固定 HTTPS endpoint。它不是 S3 IAM /
SigV4 客户端，没有 list bucket、动态 key、multipart、STS、refresh 或 region discovery。

```rust,no_run
# #[cfg(feature = "research")]
# async fn example(secret_presigned_url: String) -> Result<(), taskloom::runtime::RuntimeError> {
use taskloom::runtime::rag::{PresignedObjectConfig, PresignedObjectSource};
let source = PresignedObjectSource::new(PresignedObjectConfig {
    source_namespace: "reviewed-objects".into(),
    source_version: "v1".into(),
    object_id: "policy-object".into(),
    expected_sha256: "0".repeat(64), // Replace with a reviewed exact body hash.
    expected_bytes: 1024,            // Replace with the reviewed exact byte size.
    max_bytes: 8 * 1024 * 1024,
    timeout_ms: 5000,
    allow_http_loopback_for_tests: false,
}, secret_presigned_url)?;
let snapshot = source.load().await?;
let document = snapshot.document("policy", "政策")?;
# let _ = document;
# Ok(()) }
```

URL 是独立的 owned secret 参数，不是可序列化配置；连接和 snapshot 都没有 Debug /
Serialize 或 URL getter。source/metadata/policy hash 使用公开 logical IDs、版本、
预期 SHA-256、大小与上限，不包含 URL、签名、临时 credential、响应 ETag 或 header。
宿主必须自己保护 URL 获取、交接、进程内存和服务器日志，且不应允许模型/请求选择
URL。公开 IDs 仅接受有界 ASCII 字母数字及 `_.-`，不能塞入实际 URL。

生产只允许 HTTPS，拒绝 username/password 和 fragment。显式 fixture 选项仅允许
数字 loopback `127.0.0.1` / `::1` 的 HTTP，不接受 `localhost` DNS 或远程 HTTP。
URL 最长 16 KiB。每次显式 load 只发一次固定地址 GET，禁用环境代理、redirect 和
reqwest retry policy；默认不发送额外 bearer、cookie、Range 或跟随 Location。
合法预签名 URL 的签名校验与 object ACL 仍由对象服务负责，本模块不验证签名算法。

只接受 HTTP 200、没有 Content-Range、没有压缩 Content-Encoding（identity 可接受）
的完整 body。expected size 必须为 1–8 MiB 范围内的精确正数且不大于 max_bytes；
有 Content-Length 时必须相等，chunked 响应也在每个 chunk 后检查精确 size / 上限。
EOF 后再验证精确字节数与原始 bytes SHA-256。任何错误、短读、超时、重定向、超限
或 digest 不一致都拒绝，不返回部分对象，不把 HTTP 错误正文或签名 URL 放进错误。
连接与完整读取共享 1–60000 ms 的显式总时限；取消不触发自动重试。

snapshot 只暴露已验证的 bytes 和公开 provenance。`document(id,title)` 仅将合法
UTF-8 转成原样文本 Document；binary 可保留为 inert bytes 交给另一个显式选择的
解析器，不按 MIME、后缀或对象内容执行程序、HTML、Office、公式或下载引用。
引用 offsets 指向这份精确对象内容。服务 GET / egress 仍可能有存储厂商费用，宿主
需自行约束；这不是 generation RunBudget 或自动费用结算实现。

## 本地验收

```powershell
cargo test --locked --features database,research --test agent_sources
# Only with an explicitly owned disposable PostgreSQL fixture:
cargo test --locked --features database,research --test agent_sources -- --ignored
```

真实 SQLite 检查 namespace NOCASE 反例、唯一 ID、类型/大小/行数/完整序列化上限、
source identity 变化、冷加载不改 corpus、拒绝 view/表达式，以及并发两行事务写入下
读到一致一代。PostgreSQL 使用独占本地官方镜像 fixture、真实事务与源表，未配置
`TASKLOOM_TEST_POSTGRES_URL` 的默认忽略不能计作通过；其测试只创建/删除唯一的
owned table/view，不修改已有表。真实云数据库、复杂 schema、RLS 与生产容量未验收。
另有显式 ignored 的真实 LATIN1 单元控制，使用独占数据库
`TEMPLATE template0 ENCODING 'LATIN1' LC_COLLATE 'C' LC_CTYPE 'C'`；在实际 loader
的 metadata SQL 上验证 `é` 为 2 个输出 UTF-8 bytes（原库为 1 byte），验证精确
body 上限与成功快照。需单独的 `TASKLOOM_TEST_LATIN1_POSTGRES_URL`，普通忽略不计通过。

对象测试使用真实本机 HTTP 服务模拟 S3-compatible 预签名 GET，验证 exact bytes /
digest、chunked、redirect、服务错误、截断、Content-Encoding、Content-Range、超时、
size 和 digest 失败、每次仅一请求、URL 不进入返回数据或错误、binary 不自动转文本，
以及手动导入后的精确引用。它不是云 S3 账号、真实 SigV4 / IAM 或厂商 SDK 互操作
验收；预签名 query 字段是专门的非生产 fixture。

这两个包仍缺 CLI / HTTP / UI 宿主管理接入、数据库多表/字段映射、对象账号认证与
分页、原子批量/增量同步、删除传播及独立生产语料验证，不据此把数据生态整体勾为完成。
