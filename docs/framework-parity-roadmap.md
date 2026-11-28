# 主流框架对齐开发清单

本轮起点：`0ae4bca`。这是上一轮 P0–P2 完成后的新范围，不改写之前的
有限验收结果。用户选择先完成代码与本地验收；真实云部署、账号兼容和
独立人工标注仍需真实环境，不用本地夹具替代。每项同时记录实现和验收，
未打勾即完整原目标尚未完成。下列方框包含真实账号、生产拓扑和独立评价；
用户选择的“先完成代码与本地验收”按后面的本地交付表核对，不用本机结果
把外部验收项自动勾完。

## 必须保持的契约

`AgentTask`、最小默认 feature 和 Rust 1.86 保持；审批绑定实际操作和
精确输入，预算在每次派发前持久化，未知用量/副作用保留且不静默重试。
文档、模型、MCP server、技能和记忆均不能扩大宿主权限。旧 JSON 默认
身份不变；新增显式配置改变冻结身份后不得恢复旧运行。

## P0：效果与可靠性

- [ ] 中文/英文/混合语言、短查询、改写、跨领域及多类文档的版本化检索集；
  独立标注导入、冻结和错误/缺失分母规则；reranker 基线与真实 embedding 验收。
- [ ] 引用原文、观点支撑、事实准确性、任务覆盖分别评分；多人标注一致性及
  同模型 judge 的已知漏判保留，自动评分不能冒充人工真值。
- [ ] 统一模型/任务/输出和尝试预算的多框架对照，记录原生控制流、适配器
  差异、失败、用量与耗时；无法同条件比较的能力显式标注。
- [ ] 多机/网络分区、数据库断连、owner fencing、积压、扩缩容及长时间 SLO
  的可执行故障演练工具；本地进程/容器验证和真实多机证据分开。
- [ ] 滚动升级兼容策略与演练；不能将当前要求静止窗口的 schema 迁移称为
  滚动升级。无数据丢失、未知预算/效果不重放、回滚边界均需验证。

## P1：协议、Provider 和数据生态

- [ ] MCP stdio：显式进程/目录/环境名，bounded 协议、超时/取消/关闭与身份绑定。
- [ ] MCP resources、OAuth discovery/login、elicitation、sampling、resumable
  SSE 和会话恢复；每种能力独立 opt-in，server 不能主动取得凭证或 model 预算。
- [ ] Responses、Gemini、Azure、Bedrock、Vertex generation/tool/usage/error
  适配及 CLI/API 配置；各协议原生契约检查，真实账号验证单列。
- [ ] 语言分词、reranker、Office/邮件/数据库/对象存储加载、向量库和增量
  索引扩展；来源范围、snapshot/embedding 身份、外部费用与权限保持可审计。
- [ ] 可选语义记忆、去重/ontology/冲突审核；用户私有记忆和私有历史、
  提取运行/会话、恢复、诊断、计费和跨用户隔离必须一并解决。
- [ ] 签名技能注册表、发布者信任、版本/撤销/升级、离线验证及连接器示例。
  安装与信任不等于激活或授权，撤销不能删除历史不确定效果。

## P2：产品、SDK 和运维交付

- [ ] 正式多用户 host 命令、health/readiness、SIGTERM drain、容器与 Compose；
  Kubernetes/Helm、Secrets/RBAC、部署/升级/备份/故障操作手册及本地演练。
- [ ] 多人 UI、项目/团队协作、SSO/OIDC、用户私有记忆及权限管理；统一
  浏览器真实操作与服务身份，不能将共享项目历史声称为私人对话。
- [ ] Studio 调试/评测、实时事件、可观测 SLO/告警及审计面板；默认不上传
  prompt/secret，未配置外发时保持本地。
- [ ] Python/TypeScript SDK、分页/超时/取消/审批/恢复和错误类型契约测试。
- [ ] crates.io/注册表发布准备、版本兼容表、签名制品、模板/插件索引。
  实际发布和托管实例验收须记录真实地址/版本，不以文档代替运行服务。
- [ ] 多区域灾备与托管控制面：可部署源码、配置隔离、备份恢复、访问审计
  和实际环境验收；先本地验收，不创建隐含收费基础设施。

## 当前执行批次

以下为本轮增量状态；这是可审阅进度，不把整个 P0–P2 提前标记完成。

| 包 | 已实现及本地证据 | 仍缺的部分 |
| --- | --- | --- |
| 多语言评测与实际对照 | 28查询/8文档/8类组、标签导入/冻结/分母/四维评分/多人kappa；Python9+observed比较器4控制；实际Taskloom/原生LangGraph五算术同条件对照均2/5，真实请求/attempt/权限/限制绑定一致 | 独立人工标签、真实多人一致性、未见行业集；小型adapter结果不推导产品排名 |
| 真实 embedding 检索 | 原两轮失败保留；新增实验learned rerank首两轮也失败。显式可见集阈值校准+每文档cap1后，第三轮召回21/21、负例7/7、MRR.929、0错误；原标签/通过门槛不变，29embedding+51rerank各自记账 | 校准使用了同一可见开发集；独立未见语料/人工效果仍未验收，不能称默认检索普遍达标 |
| 原生 Provider | Responses/Gemini/Vertex/Bedrock bearer/Azure generation；Gemini/Vertex embedding；真实本机 HTTP 契约测试；App index 支付前预留已测 | 真实云账号、AWS SigV4/自动凭据轮换、其余原生 embedding、流式协议 |
| MCP | 原协议组36项；独立mcp-persistence已冻结，SQLite4/真实PG1：原POST fsync后强杀/GET接管、Observed先保存、竞争/fencing、SQL失败poison、9类负例及ACK tombstone；独立隐私审查通过 | 真实第三方互操作；持久v1只支持HTTP原session，明确拒callbacks/OAuth helper组合，不承诺全局exactly-once |
| 检索生态 | CSV/JSONL/UTF-8 EML、DOCX/XLSX/PPTX、SQLite/真实PG只读源、精确对象GET、独立rerank账本、Qdrant SDK及App/CLI、exact-chunk增量SDK；真实英中OCR及超时/取消容器控制已通过 | 真实广泛语料效果；更多格式/MIME、自动外部同步和App自动增量属于继续扩展，当前受支持子集见各文档 |
| 私有数据 | SQLite/真实 PostgreSQL 的 owner 隔离、私有会话及子运行、提取、配额共享/原子预留；真实浏览器 Alice/Bob 拒绝控制 | 私有写工具、多副本私有调度、生产容量；私有执行容量与共享队列容量独立配置 |
| 语义记忆 | 显式有界SDK、审核过滤、快照/owner/model绑定、并发遗忘丢弃、只读ontology建议；显式metered HTTP/API/SDK/UI入口；SQLite/真实PG及浏览器控制通过，付费前envelope检查已独立审查 | 真实语义效果；持久化增量memory vectors为继续扩展；普通记忆操作不自动上传 |
| 签名技能 | Ed25519、公钥信任/撤销、精确归档校验、版本水位、离线 CLI/SDK；Python 签名→Rust 安装实测；App/SQL/CLI 强制校验、lost-ack/pending 恢复共11测试 | 跨机可信注册表和实际公共托管；本地文件 ledger 与 SQL 不宣称同一事务 |
| 服务部署 | 正式host/check/health、SIGTERM、Compose/Helm；Rust1.86最终全opt-in源码镜像a107111...，实际Compose7项通过并清理；使用校验公共crate离线编译+已审阅runtime依赖层，网络构建超时失败不隐藏 | 真实Kubernetes/ingress/TLS；自动HPA与跨schema热迁移不由本地Compose证明 |
| 登录与工作台 | 可选中文console；固定JWKS OIDC、PKCE/state/nonce、HttpOnly cookie/CSRF、实时SQL权限；fake IdP8测试；真实Alice/Bob、私有语义入口、11条事件/精确cursor、断开草稿清理与390px浏览器验收 | 外部IdP/TLS；浏览器会话目前单进程；共享session/JWKS热轮换与跨项目统一管理为继续扩展 |
| 观测 | 无prompt/secret/配额自由文本的认证项目指标；真实Prometheus3.16.0原规则parser/三项虚拟时间测试通过；实际host授权/匿名scrape、禁用告警/本机协议receiver/恢复两轮闭环 | 实际Alertmanager路由/去重/静默；生产长期SLO。实时演练的2秒等待不冒充原5分钟阈值 |
| SDK | 最终Python5/Node5/strict tsc、实际wheel/npm tgz/dry-run通过；从实际归档加载semantic方法2次HTTP契约通过；双license/README/无cache与选定凭据模式检查通过，14源文件前后SHA一致 | 实际注册表发行需真实身份；全部API/更多语言为继续扩展；npm private:true |
| 故障与恢复 | SQLite/PG进程死亡/unknown/效果/fencing/积压/DB lock/pause/备份19项两轮；最终ELF真实TCP切断/重连/实际epoch1→2/手动1→2→1共7项，16/16健康任务、故障1、恢复后0额外请求 | 单物理主机；分区恢复样本1止于保留unknown安全停止，不计业务完成SLO；真实多机与长期SLO另验 |
| 有限滚动回退 | 最终ELF8e996d20...独立重跑：全局3、SQLite13/PG13检查，各13健康job/HTTP、真实old/new并发、drain/旧版回读并继续新会话与workflow、未知不变/错配拒绝；owned PG清理 | 仅指定旧新、同schema/同冻结合同；跨schema继续静止路径，不承诺任意旧版rolling |
| 人工静止DR控制面 | 最终a107111...镜像、真实独立3PG及managed worker：23/23，离线8/8；独立witness/CAS/单调epoch、永久NOLOGIN+terminate、9类资产备份恢复、实际Application binding、A→B→新A、不可达拒绝、lostACK→Unknown及精确清理 | 显式本地受管Docker子集，无自动云资源/路由；源区不可达且无法fence时拒绝晋升；不称生产跨区域HA |
| 发行准备 | source bundle/checksum/独立Ed25519签名与显式外部trust验证工具9项真实临时repo/ephemeral-key控制；兼容矩阵、中文默认README/英文入口、最终SDK/镜像及公开sanitized验收摘要均有独立证据 | clean source artifact按实际Git树准备；真实维护者签名/注册表身份另列，测试key不作为维护者身份 |

最终Rust回归：Windows **510通过/31忽略**、Linux **512通过/31忽略**；两平台
最小feature各84通过。两平台差异来自平台专有控制，忽略不算通过，真实PG/OCR等
按独立回执验收。Rust1.86全feature/all-target检查及1.86镜像源码编译通过；
两平台strict Clippy、Windows warnings-as-errors rustdoc通过。首轮Linux负载下2秒测试失败，单独及threads2全量重跑
通过，未修改deadline；Windows首次缓存E0463与后续源码漂移中的停止批次均不算
通过，最终在独立低debug目录完整重跑。Python/文档后续变化与Rust输入稳定分开记录。
本地 target 文件不作为开源制品发布，最终可公开验收应记录源码哈希和范围。

已落地入口：[Provider](native-providers.md)、[Embedding](native-embeddings.md)、
[MCP](mcp-extensions.md)、[检索](retrieval-extensions.md)、
[Rerank](reranking-provider.md)、[私有会话](private-sessions.md)、
[语义记忆](semantic-memory.md)、[注册表](skill-registry.md)、
[部署](production-deployment.md)、[OIDC](oidc-service.md)、
[浏览器登录](login-service.md)、[SDK](client-sdks.md)、
[工作台](team-console.md)、[项目指标](operational-metrics.md)、
[故障演练](operational-fault-drills.md)、[评测](parity-evaluation.md)。

新增最终入口：[持久MCP](mcp-durable-recovery.md)、[有限rolling](same-schema-rolling.md)、
[网络/容量](network-capacity-drills.md)、[受管DR](managed-disaster-recovery.md)、
[兼容矩阵](source-compatibility-matrix.md)、[源码发行](source-release-preparation.md)。
