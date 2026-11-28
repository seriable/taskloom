# 源码发行兼容矩阵

这是当前 **受限源码合同**，用于发行准备，不是全平台/云厂商/生产认证清单。
Rust package 当前为 `0.2.0-alpha.1`；Python SDK 为 `0.1.0a1`，TypeScript/JS SDK
为 `0.1.0-alpha.1`。这些版本号不证明跨版本持久状态兼容；最终发行需绑定实际源码与
二进制 hash、启用 feature、配置和独立验收 receipt。

## 运行时、语言与 feature

| 范围 | 声明与支持子集 | 不推导的兼容性 |
| --- | --- | --- |
| Rust 最小库 | Rust **1.86+**、edition 2021、`default=[]`；typed AgentTask/Provider API | 开启所有 feature、不匹配下游依赖锁或更旧编译器自动兼容 |
| `schemars` | 显式 `TaskSpec::schema_of` 类型schema生成 | 任意schema等于现实正确性、模型能遵守或输出自动获得权限 |
| `openai` / `anthropic` | 显式 HTTP provider；native 五类协议通过 `openai` opt-in；原价格/预算/unknown合同 | 所有参数、模型、专属推理状态、云账号或自动凭据链 |
| `runtime` | durable run/session/workflow、显式审批/恢复、RAG、exact-chunk 增量 SDK | 任意旧/新 snapshot、changed model、source或schema可混用；App自动增量 |
| `database` | SQLite / PostgreSQL，schema **v3**、显式 migration、namespace/lease/queue/quota | 任意数据库、任意schema、跨区split-brain、自适应热迁移；SQLite不是多机共享数据库服务 |
| `app` | 明确配置的 API/CLI/operator视图、多用户host按可选database集成、受控工具与费用 | 公网托管/认证服务自动部署、任意目标/SQL/文件读取；编译feature不等于启用服务 |
| `mcp` | 固定 HTTP或显式stdio；resources、固定OAuth公共客户端、host-reviewed sampling/form、显式有限SSE GET | 所有MCP server/版本、自动重POST、后台交互；旧单进程API不自动获得跨进程恢复 |
| `mcp-persistence` | 独立host SDK、SQL owner/epoch、原HTTP session有限SSE GET、Observed/ACK tombstone；SQLite/PG真实跨进程控制已验 | 不恢复stdio，不重POST/替代session；v1静态拒绝全部McpHttpExtensions，不自动采用到RunStore、不证明远端幂等 |
| `oidc` | 固定issuer/aud/JWKS/subject映射，RS256/EdDSA；PKCE/state/nonce/cookie/CSRF浏览器子集 | 不含jti的所有IdP、动态discovery/key轮换、refresh、跨副本session、远端logout自动传播 |
| `skill-registry` | 本地Ed25519 trust/receipt/版本水位与撤销；显式App/SQL策略，原grants不扩张 | 维护者源码发行签名、公共托管registry、跨机器原子trust/热撤销 |
| `office` | 受限transitional OOXML DOCX/XLSX/PPTX，bounded ZIP/XML，显式可信本地CLI/SDK | Office渲染、宏/公式求值、完整格式/编码、notes/master/OCR自动处理 |
| `research` | host-granted Web/PDF、HTTP rerank、固定预签名对象快照 | 任意网络/服务器路径访问、S3 IAM/SigV4、自动外部source同步、免费外部服务 |
| `qdrant` | 固定namespace/index/source绑定的真实REST、显式journal发布/receipt；App接入单列其最终回执 | 远端文本代替原证据、自动query upsert、分布式collection/原子corpus发布或生产容量 |
| `sandbox` / `ocr` | 固定Linux容器合同，无宿主shell fallback；OCR另有typed PNG/Tesseract降权worker | VM/内核隔离保证、所有平台/图像格式、真实扫描准确率或取消后必定已清理 |
| `telemetry` | 显式目的地与原scope的有界redacted OTLP/HTTP；默认无外发 | 未配置自动上传、无限事件/高基数metrics、生产长期SLO或云可观察平台 |
| Python SDK | Python **3.10+**；同步bounded HTTP、明确timeout/错误、精确审批、显式恢复/取消、事件cursor及private/semantic入口 | 自动重试、不限量列表分页、所有API typed返回、Python3.9或async客户端 |
| TypeScript/JS SDK | Node **20+**内置fetch、ES modules、显式timeout/AbortSignal与事件cursor；拒绝不安全JS整数 | u64无损Number、无限分页、自动恢复/重试、所有browser/runtime兼容；npm目前private:true |
| 发行准备工具 | Python **3.11+**、Git、本机已装Ed25519 cryptography；raw source与离线签名profile v1 | 自动建立维护者身份、验证真实人类、签名即安全/质量、自动发布或源码里发现trust |
| 受管灾备 v1 | Python/Docker显式owned两区域PostgreSQL+第三独立witness；人工静止全量检查点、role/session fencing、新epoch/新role/db反向晋升；实际receipt单列 | 源不可达时自动提升、任意SQL目录/资产根、多主机地域/WAN/Kubernetes、远程object/vector全备份；Unknown不自动补偿或重放 |

本表不替代 Cargo 的依赖解析：保持受审阅的 `Cargo.lock`，最终按各feature组合实际
编译和执行测试。源码发行工具只核对声明MSRV/default[]，不会据此宣称编译已通过。

## Provider 与协议限制

| 协议 | 当前显式子集 | 需单列的边界 |
| --- | --- | --- |
| OpenAI-compatible Chat / Anthropic Messages | 现Provider与runtime工具/usage/审批合同 | 账号、具体模型、参数/价格；不得跨模型隐式fallback来掩盖不同vector space |
| Responses | 非流式文本/宿主函数工具，store:false | 不可表示的reasoning/encrypted状态拒绝，非全部推理模式兼容 |
| Gemini / Vertex | 非流式文本/原生函数工具；原生embedding单次有界请求 | thoughtSignature拒绝，Vertex需显式预发Bearer；没有ADC/metadata-server自动凭据获取 |
| Bedrock | 显式Bearer token的Converse子集 | 非AWS SigV4/STS；typed structured schema不假装全模型支持 |
| Azure | 固定deployment/api-version，model精确匹配部署，显式计价 | 无部署发现/API版本自动切换/Entra凭据刷新 |
| MCP | 文档化2025-06-18/2025-03-26子集；elicitation仅支持前者form | 无URL elicitation或自动sampling后端；每个opt-in独立授权；MCP不是工具幂等性证明 |
| JSON service / SDK | 现有项目scope路径与精确run/ticket/revision/cursor字段 | 没有宣告所有服务端未来字段/版本兼容；1000记录边界与events分页分别报告 |

协议fixture通过只证明请求/响应、身份/费用/状态与拒绝边界。真实云Provider/IdP、
区域/配额/订阅价格、第三方MCP server和生产TLS仍未被本矩阵认证。

## 持久状态与升级

| 状态 | 必须保持的绑定 | 兼容/恢复边界 |
| --- | --- | --- |
| Run / session / workflow / queue | 原模型/工具/role/输出/schema/grants/预算/源身份与实际owner/epoch | 新配置改变冻结合同拒绝旧运行；未知用量/效果必须保留、显式核对，不自动退款/重放 |
| SQL schema v3 | 实际数据库版本、dataset、namespace、lease与queue policy | schema变化仍需全部静止与显式迁移；same-schema old/new rolling仅以指定实际二进制与回执证明，不由本表预先通过 |
| EmbeddingIndex v1/v2 | 完整CorpusSnapshot、model identity、维度与document_context | v1 body-only默认false保持旧JSON省略；v2有title context；增量要求完整旧snapshot，clear-all返回None供宿主显式处理 |
| 私有记忆/历史 | 已认证tenant/project/owner、审核状态、scope snapshot及共享项目quota | 私有no-tools合同，不把shared history改名即当private；临时semantic index不自动持久化或学习 |
| Registry / Qdrant / rerank等独立ledger | 各自完整target/index/trust/费用绑定、pending/complete状态 | 不等于全部属于SQL事务；部署/备份必须包含实际启用的文件状态，多副本/跨区另行设计 |
| Source release v1 | 实际clean HEAD commit/tree、raw Git files、精确artifact/manifest/checksum、显式外部公钥 | 不根据archive内key建立信任；不同HEAD/dirty checkout/不同domain拒绝；不替代代码或质量审核 |
| Managed DR witness | 第三独立物理PG的完整状态CAS、单调epoch、Prepare/Active/Unknown审计；九类完整私有资产及原生full dump | witness不能随region clone恢复；永久禁用历史role；目标实际Application frozen queue binding须一致；本地fixture实测不证明生产RPO/RTO |

当前没有“任何旧版本均可读取所有新状态”的承诺。Rust/SDK package version、SQL schema、
文档/索引/签名wire format和宿主冻结合同是不同维度，不能互相替代。

## 真实验证仍需外部身份或环境的内容

独立人工标注/仲裁、实际云模型/IdP与用户套餐计价、真实多机/多区域/Kubernetes与
长时间SLO、维护者签名身份和crates.io/PyPI/npm/公共publisher发行均单列未验证。
已有真实模型检索未通过的冻结门槛仍是未达标结果，不因签名、测试数量或兼容声明改为通过。

发布者应另行公开绑定最终源码的sanitized验收receipt；原始target下的凭据、私有
数据库/会话、模型内容和测试私钥不进入source制品。测试ephemeral key签名不能冒充维护者签名。
