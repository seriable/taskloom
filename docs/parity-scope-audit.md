# P0–P2 范围与剩余工作独立审计

早期审计日期：2026-11-28。对象是本轮工作树及
[主流框架对齐清单](framework-parity-roadmap.md) 的 17 个原始条目。
这是一份范围审计，不是最终源码冻结回执、竞争排名或完成百分比。
部分包仍在最终集成 / 真实容器验收；下文只承认已经读到的对应日志，不能把旧的
全量通过覆盖后续新增源码。没有改变原清单、旧验收数据或旧门槛。

早期结论：当时已经形成较完整的本地 durable agent 内核、受控服务和若干真实连接器，
但“所有 P0–P2 已完成，剩下只是云账号”不成立。存在可在本地继续开发与验证的
源码缺口；另有真实效果门槛未通过，不能用协议测试数量抵消。

## 后续闭环更新

下方详细表保留本次独立审计时的观察；以下后续实际证据优先，不能继续把已经
补齐的子项记为“尚未实现”。它们仍是候选二进制证据，不是最终源码冻结回执。

最终本地收口：MCP durable SDK已独立SQLite/PG硬杀与SQL失败验收；最终Linux
ELF的rolling/真实TCP/手动容量矩阵已重跑并清理；Rust1.86最终镜像的Compose7项、
第三PG witness人工DR23项和8项离线控制通过，源码签名准备9项与SDK实际归档通过。
最终Rust全功能Windows510/Linux512、各最小84通过；31个ignored保持单列。
新可见开发集校准profile召回21/21、负例7/7、MRR.929通过原门槛，旧失败保留。
以上不证明独立未见效果、外部账号、生产多机/多区域/长期SLO或真实维护者签名。
最终公开摘要见[本地验收回执](acceptance/framework-parity-local-20261128.json)；
下表中仍写“尚缺”的旧时点描述须结合该回执及[当前交付表](framework-parity-roadmap.md)。

- **有限 rolling**：实际旧 `0ae4bca` 与候选新 Linux ELF 的 SQLite/PostgreSQL
  两轮并发、drain、回滚及错配拒绝已完成；第二轮各13检查、各13个单claim健康
  job及13个唯一HTTP marker通过。未知预算/已落盘效果冷读保持，见
  [有限滚动合同](same-schema-rolling.md)。新MCP最终ELF需另行绑定验证。
- **真实监控**：真实Prometheus3.16原规则parser及三项虚拟时间测试通过；
  实际host授权/匿名scrape、独立bearer保护的本机receiver、禁用告警及恢复共13项
  通过。实时等待显式缩短为2秒，不等于生产阈值/长期SLO或完整Alertmanager路由。
  见[项目指标](operational-metrics.md)。此前直接下载TLS失败仍保留为失败记录，
  成功证据来自明确记录digest的第三方镜像。
- **同模型实际比较**：候选Taskloom与原生LangGraph1.2.24在原5个算术案例下
  实际执行，两者均2通过/3失败/0未执行；实际wire上下文、尝试计数和记录的
  model/tools/grants/limits/approval/context合同一致。严格比较器另有4项负例控制，
  不能据此得出一般质量、速度排名或DeerFlow/agentUniverse产品等价结论。
- **发行源码准备**：独立source bundle/checksum/Ed25519签名与显式外部公钥验证
  工具的9项临时repo/ephemeral-key测试通过，兼容矩阵已有；见
  [发行准备](source-release-preparation.md)。最终clean Taskloom artifact、公开
  sanitized receipt与真实维护者签名尚不由这些方法测试代替。
- **OCR/Qdrant/语义UI**：真实英中OCR与取消清理、Qdrant SDK及App真实SQLite/PG
  组合、显式语义HTTP/API/SDK/UI与浏览器事件均已补充实际回执；详见主清单新表。

MCP持久会话、人工静止DR、网络隔离/副本替换已按上方最终本地合同闭环。
多语言原两轮未过的结论不变；后续新profile的校准结果不能覆盖旧失败，
也不能将可见开发集标成独立未见评测。

## 判定口径

- **已闭环子项**：有实际实现与对应真实本地协议 / 存储 / 进程 / 浏览器测试。
  fixture 模型只证明控制，不证明模型效果；真实容器服务也不等于真实云账号。
- **环境 / 选择缺口**：已有明确受支持合同，需要实际账号、目标部署、独立人类或
  发行身份才能获得证据。不能由 Agent 伪造；也不能借此把没有实现的源码归成环境问题。
- **源码缺口**：还需实现状态、协议、管理入口、脚本或兼容策略，提供账号本身无法补齐。
  SDK 子集与产品集成分别判定。通用 trait、模板、说明或静态配置不是整项完成。

规模只用于区分工作性质：**小**为单模块 / 工具的明确闭环；**中**为多个 API、
存储与 UI / SDK 的合同变化；**大**为跨进程、版本兼容、认证或分布式状态协议。
不据此承诺工期。某厂商 SDK 的全部可选功能不自动成为本清单的硬要求；本轮可按
准确声明的受支持子集验收，扩展范围需与原目标区分。

## 原文对应的最小必要 gate

下表用于判断原范围是否已闭环；后面的“未来扩展”不能反过来自动增加本表门槛。
尤其：私有写工具、SSO 热轮换/多副本、全 API 类型、App 自动增量和外部原子同步，
都不是因为出现在限制说明里就成为原条目硬要求。

| 原文必要子项 | 最小 gate 与当前判定 |
| --- | --- |
| P0.1 版本化多语言集、独立标签导入/冻结/分母、reranker、真实 embedding 验收 | 方法与集代码已有；真实结果必须按冻结门槛报告，现有两轮未全过，效果 gate 尚未通过。无需为了此项先实现所有格式 loader |
| P0.2 四维评分、多人一致性、judge 漏判保留 | 方法工具与分母/一致性代码已闭环；真实人类与漏判数据仍需真实标签/输出，标注 Web 平台不是硬要求 |
| P0.3 统一条件多框架对照 | 对所选至少两个实际原生框架执行，冻结条件、实际流量/attempt/grants/outputs/usage/错误分母可关联；不可比能力准确单列。现有声明比较器不替代这次执行；不自动强制新增所有竞品 adapter |
| P0.4 可执行多机/分区/断连/fencing/积压/扩缩容/SLO 工具 | 已有真实本地多进程及故障包；仍缺网络层隔离/恢复、受控副本替换/扩缩容与相应度量的可执行本地闭环；实际多机器/长时间生产数据另列环境 |
| P0.5 滚动升级兼容策略与演练，静止 schema 不冒充 rolling | 可限定 **同 schema + 同冻结运行合同**，真实旧/新二进制并行、drain/替换/rollback与未知账本不重放；不兼容 schema/config 明确拒绝并走静止迁移。这足以形成准确的有限 rolling 合同，不要求任意跨 schema 热迁移。当前此演练尚缺 |
| P1.1 显式 bounded MCP stdio | 当前代码与真实子进程控制已闭环；无需把 detached descendants / VM 隔离加为此项门槛 |
| P1.2 opt-in resources/OAuth/elicitation/sampling/SSE/session recovery | 当前 SDK 单进程协议子项有36项本地证据；本轮要求的持久/跨进程会话恢复仍缺，不能把内存 cursor 说成该能力。无需自动把所有 SDK 能力做成面向模型的 App API |
| P1.3 五类原生 generation/tool/usage/error 与配置 | 所声明非流式原生子集代码/本地 HTTP 已闭环，真实账号兼容另列；SigV4、streaming、推理私有状态等是未来兼容扩展，不是为勾原条目自动增加的全部要求 |
| P1.4 语言/rerank、Office/邮件/DB/对象、向量库与增量索引扩展 | 命名类别已有实际实现与局部真实验收；**增量 SDK 满足“索引扩展”源码子项，不要求先给 App 自动增量**。OCR若计入本轮新增则需其真实容器结果；更宽格式/外部原子同步不自动成为此项 gate |
| P1.5 可选语义/ontology建议/冲突审核、私有历史/提取/恢复/诊断/计费隔离 | 当前受限 private no-tools + 显式语义 + 原审核CAS的代码和 SQLite/PG/浏览器控制已闭环；私有写工具、自动 merge、持久 memory vectors 不作为追加硬要求 |
| P1.6 签名 registry/trust/版本撤销/离线验证/示例 | 当前 local/offline registry + App/SQL/CLI 强制校验已闭环；真实 publisher/公共托管与分布式 trust 属于发行/托管选择，不阻止本机离线合同成立 |
| P2.1 host/health/drain/Compose/Helm/Secret/RBAC/手册/本地演练 | 源码/模板和早期真实演练已有；最终源码镜像/Compose/本地部署检查需绑定最终 binary。真实 Kubernetes 属目标环境；无需把所有可选本地 ledger 集成变成默认 SQL host 的新 gate，但启用时必须准确声明状态部署方式 |
| P2.2 多人UI/项目共享/OIDC/私有记忆/权限 | 当前显式子集代码与本地 IdP/HTTP/浏览器控制已形成闭环；真实外部 IdP/TLS 缺证据。共享会话持久化/JWKS热轮换/协同编辑不是追加必要条件 |
| P2.3 Studio调试/评测、事件、SLO/告警/审计视图、默认不外发 | 已有设计/Trace/本地评测比较、项目console/事件和metrics/alerts；还需实际本地规则/监控演练及最终浏览器/集成回执。跨项目统一管理台不是原文必须，生产长期SLO另列 |
| P2.4 Python/TS、分页/超时/取消/精确审批/恢复/错误契约 | 当前事件cursor分页、明确有限集合、核心方法和传输控制已有真实HTTP契约测试；不要求补齐所有集合分页、所有API方法和完整typed返回才能接受该受限SDK合同 |
| P2.5 发布准备/兼容表/签名制品/模板索引 | source packaging、SDK打包与技能模板已有；还需最终公开可复核的 source receipt、明确兼容表及发行source签名/验证准备。实际维护者签名/注册表发行需要选定身份；源码开源不要求擅自发布二进制或托管服务 |
| P2.6 多区域DR与托管控制面源码/配置隔离/备份/审计 | 仍缺可执行的控制面/区域状态与promote/fencing/全资产恢复协议，不只是缺云账号。可先本地两组进程/数据库真演练，并明确不是已部署真实区域；自动云资源创建不属于默认授权 |

## 实现证据与尚未实现的能力

本表列出完整边界；“尚未实现”同时包含必要 gate 和未来成熟度扩展。
是否阻止原项闭环，必须回到上一表判断。

| 原条目 | 已实现并有明确本地证据的子项 | 仍需环境 / 实际证据 | 仍有源码或产品闭环缺口 |
| --- | --- | --- | --- |
| P0.1 多语言 / 多领域检索、标签与真实 embedding | 8 文档 / 28 查询 / 8 类可重叠组；冻结 hash、独立标签导入、严格错误分母；lexical / HTTP rerank；已有真实 Qwen embedding 请求 | 独立人工标签、未见资料与更广文档样本；已有两轮真实检索没有通过全部冻结门槛，必须保留失败 | 完整 held-out campaign 与可复现错误归因流程仍需闭环；真实召回问题需校准 / 实现修复后在新冻结集验证，不能只改标签 / 放宽阈值。受支持 loader 格式不等于已进入该效果集 |
| P0.2 四维评分、多人一致性及 judge 偏差 | exact provenance / source support / corpus factuality / task coverage 分开；导入原输出 hash；pairwise kappa、coverage 与缺失 / 冲突规则的离线控制 | 真实独立评审与仲裁、真实多人一致性；同模型 judge 的漏判需要真实输出分析 | 当前方法工具不能自动核实人类身份或得出独立真值；若要成套评审交付，还需匿名分配 / 仲裁记录 / 冻结导出流程及与 judge 错误的逐案对照。不是再加一个“总分” |
| P0.3 同模型 / 同任务 / 同预算多框架对照 | 严格 conditions manifest 比较器，任何差异为不可比；已有 DeerFlow / LangGraph 的原生控制流适配与历史有限比较工具 | 真实固定版本竞品及统一条件运行；现有历史结果不能自动升级为新完整条件下的结论 | 新 comparator 仅比较声明，尚无对全部新条件逐项取证的统一原生 campaign 闭环。需把请求 / attempt / grants / outputs / error denominator / usage 与实际 manifest 关联；agentUniverse 的真实原生对照也未形成同等证据 |
| P0.4 多机 / 分区 / DB 断连 / fencing / 积压 / 扩缩容 / SLO | SQLite / owned PostgreSQL 的真实多进程死亡、未知预算、已落盘非幂等效果、owner fencing、积压、DB lock / pause、五表静止备份恢复；有限负载与进程控制有回执 | 不同机器、真实网络分区、DB HA 切换与持续时间 / 流量明确的 SLO | 可执行多节点分区与恢复 / 扩缩容工具、实际队列延迟 / 恢复指标汇总仍缺；目前本地 SQL epoch 注入不是网络分区，CPU HPA 模板不是扩缩容验收 |
| P0.5 滚动升级 | 明确的静止升级 / 备份 / 恢复流程，旧实例与 live lease 拒绝边界 | 两个具体发行版本的目标部署演练 | 当前缺同 schema/同冻结合同的可执行 compatibility gate 与真实滚动/drain/rollback；可以先实现这一有限合同。跨 schema expand/contract 是未来扩展，保留静止路径不阻止有限 rolling 合同成立 |
| P1.1 MCP stdio | 显式程序 / cwd / 环境名、bounded actor / queue、超时 / 取消 / 关闭、真实子进程及 durable 未知效果不重放 | 第三方具体服务器互操作 | 在所声明单进程 transport 子集内已闭环；不能据此声称 detached descendants / VM 隔离或所有 MCP server 兼容 |
| P1.2 MCP resources / OAuth / elicitation / sampling / SSE / session 恢复 | resources list/read；固定公共 OAuth discovery + PKCE 一次交换；精确 host callback 审批；有限 SSE 与显式 GET 原请求恢复；本地协议组 36 项通过，坏 final frame 不提前推进 cursor / 删除 pending | 真 OAuth 发行者 / 第三方 session server 的协议互操作 | 持久化 / 跨进程 session、cursor、callback unknown 与 credential-reference 恢复尚未实现；stdio 无重连，SSE 不支持多副本接管 / 后台独立请求流。App 并没有自动把全部 SDK 扩展开成用户功能 |
| P1.3 原生 Provider 与配置 | Responses / Gemini / Vertex / Bedrock bearer / Azure 的非流式文本、函数工具、usage / error 与 App 配置；Gemini / Vertex embedding；真实本机 HTTP 控制 | 受支持合同的真实账号、区域、配额、模型版本、价格 / 订阅配置 | 所声明非流式子集可单独验收；完整云 SDK 体验仍缺 AWS SigV4 / STS、自动凭据刷新、原生流式和若干专属状态。Responses reasoning / Gemini thoughtSignature 当前 fail-closed，不是仅缺账号，也不应据此说完整兼容 |
| P1.4 检索、Office / 邮件 / DB / 对象、向量与增量 | lexical 配置 / rerank；CSV / JSONL / 受限 UTF-8 EML；DOCX / XLSX / PPTX 真实 ZIP；SQL SQLite + owned PG；显式预签名对象 HTTP；Qdrant 真实单机 REST；exact-chunk 增量 index SDK。各包都有明确拒绝边界 | 真实多格式语料 / OCR 准确率、源数据库 / 对象账号 / 云向量服务；OCR 镜像及真实测试在审计时仍收敛 | App 完整旧 snapshot 配对与增量发布、外部 source 原子批量 / 删除传播 / cursor 同步仍缺；SQL / 对象 / OCR 尚无可信 App 管理工作流。MIME/编码、复杂 Office、OCR 多格式是额外格式能力，现有子集不可声称全覆盖 |
| P1.5 私有记忆 / 历史 / 提取 / 恢复 / 隔离 | 私有 SQL namespace 与共享项目原子 quota；私有 session / child run / 提取、诊断隔离、实际 Alice / Bob 浏览器控制；语义 SDK + metered HTTP / UI 显式入口，SQLite / owned PG；只读 ontology 建议 | 真 embedding 语义质量、真实长期私人资料与容量 | 私有执行为 no-tools-v1，本地 controller jobs 及并发限额不是跨副本 queue；持久化增量 memory vectors、ontology 审核 UX / 持久决策与私有工具审批恢复尚缺。只读建议不是自动 merge 或完整私有 agent 工具执行 |
| P1.6 签名技能注册表 | Ed25519、原包 hash、信任 / 撤销 / expiry / sequence watermark、离线作者工具、CLI/SDK 与 App/SQL 强制 root/dependency 校验；lost-ack pending 与显式恢复；原权限不扩张 | 真实 publisher 身份 / 公共托管索引 / 发行 key 生命周期 | 在受保护的本机 ledger 与离线 registry 子集内闭环；跨机器统一 trust / hot revocation / 全局事务没有实现。它们属于托管 / 多副本目标的额外状态协议，不能把 local file lock 当分布式服务 |
| P2.1 正式 host / health / drain / Compose / Helm / 运维 | 正式多用户 host、live / ready、SIGTERM drain、non-root 容器、Compose / Helm / Secret / 网络策略 / 显式 migration 脚本；有过实际本机 / Linux host 与早期镜像验收 | 最终源码镜像重建 / Compose 闭环、实际 Kubernetes 与所选 ingress / TLS / Secret 环境 | Chart / RBAC 拒绝规则是源码，集群部署验收脚本与故障 / 升级工具仍须覆盖目标拓扑；Recreate 不能宣称 rolling。可选本地 ledger 状态还要纳入挂载、备份和多副本设计，不能泛称所有状态均在 SQL |
| P2.2 多人 UI / 协作 / SSO / 私有记忆 / 权限 | 项目共享工作台、SQL 身份 / role 实时检查、独立私人视图；固定 JWKS bearer + PKCE / nonce / Cookie / CSRF；真实本机 fake IdP / 浏览器与草稿清理 | 满足当前 claim / 算法子集的真实外部 IdP、TLS cookie 回跳、实际浏览器环境 | SSO session 为进程内，缺多副本共享与 IdP discovery / JWKS 自动轮换 / refresh / 远程 logout 生命周期；身份 / 项目管理主要可信 SDK / CLI，尚无完整多人管理 UX。现有共享阅读与精确审批不是协同编辑 / 全部团队产品能力 |
| P2.3 Studio / 事件 / SLO / 告警 / 审计 | 已有 operator 设计 / Trace / 本地评测文件比较视图，中文多人工作台、30 秒有界 events 轮询、项目 metrics 与告警 YAML；实际浏览器已观察 11 条事件并核对身份清理 | 目标监控实例与长期 SLO / 警报触发证据；当前 promtool 下载日志为 TLS 超时，不能算已加载告警通过 | 多用户权限下统一的调试 / 评测 / 审批 / 审计产品入口与有出处的 project 汇总仍缺；项目指标主要 gauges，不是排队 / 执行 / 恢复时间和未知费用的完整 SLO 仪表。全局管理统计需另行限定授权与隐私 |
| P2.4 Python / TypeScript SDK 与分页 / 取消 / 恢复 | 真实 loopback HTTP 契约、请求一次、JS AbortSignal、Python socket timeout、错误分类、精确审批 / 显式恢复、事件 cursor、私有 / semantic 入口；wheel 与 npm dry-run | 真 Rust 服务 / 浏览器边界的 SDK 端到端组合与实际 registry 发布 | events 分页不等于全部列表分页；runs/workflows/sessions 等 API 当前 1000 上限，没有完整稳定 cursor 与对应 SDK。许多返回仍为 JsonValue，完整方法 / 错误与 API 版本合同覆盖仍不足 |
| P2.5 发布准备 / 兼容表 / 签名制品 / 索引 | Rust source packaging 规则、双许可证、README 中英入口、Python wheel / npm pack 准备；技能 package 签名工具与模板示例 | 实际 crates.io / PyPI / npm / 公共 registry 账号、发行身份及选择；源码开源不要求偷偷发布二进制 | 最终 sanitized 验收 / source hashes、版本兼容矩阵、发行 source bundle / checksum / 签名与验证流程仍须统一；技能 Ed25519 签名不等于框架发行制品已签名。npm 当前 private:true，没有实际公开 SDK 发行 |
| P2.6 多区域灾备与托管控制面 | 多用户 SQL 隔离、冷 host 配置、可信 admin CLI、同版本静止五表备份恢复与访问控制原语 | 两个实际区域、目标基础设施 / 密钥服务 / 数据库 HA 与故障切换 RPO/RTO | 没有托管 tenant/project 生命周期控制面、跨副本配置 / secret reference 分发、跨区 active-owner fencing、DR promote / rollback 协议和全资产恢复工具。两个从同一旧备份启动的独立 SQL 都可能各自认为 lease 有效；本机 lease 不解决跨区 split-brain |

## 必要工作与未来扩展的规模、最小验收

以下都是可实现的源码工作，但不全部属于原条目的必要 gate。标注为“未来扩展”的
项目不应阻止上一节已经闭环的窄合同。相应本地通过不能冒充真实多机 / 多区域 / 生产 SLO。

| 工作 | 必要性与规模 | 最小真实本地验收 |
| --- | --- | --- |
| App 增量 index 版本包与 CAS（未来扩展） | **中**。保存旧 index 对应完整 snapshot、冻结新 corpus / model / context 与费用边界、原子发布配对及 clear-all；SDK已满足原“索引扩展”，只有希望App自动使用才需此包 | 真实 SQLite / PG 与 metered HTTP：未改 0 embedding、仅变更 chunks 调用、源码改变拒绝；构建时并发 ingest、持久化确认丢失 / kill、重启后读者只见完整旧或新配对；原未知 reservation 保留 |
| 外部来源原子同步（未来扩展） | **大**。读取 source snapshot、生成新增 / 替换 / 删除计划、基准 revision / scope、单事务应用与恢复；逐文档 ingest 可能只完成半批；不要把其缺失当作没有只读加载器 | 真实源 SQL / HTTP 对象 / corpus 进程：中途死亡 / 断连 / source version 改变 / 删除；无静默丢行、其他 namespace 不改、部分结果不对读者生效，未知提交可核对 |
| durable MCP 接管 | **大**。原 request / session / cursor / processed-event / callback unknown / credential reference 的持久状态与唯一 owner；不能把 token 明文或一次 POST replay 当恢复 | 两个实际 client 进程 + 真 HTTP/SSE server；原效应后杀 client，由另一进程显式 GET 原 request；effect POST 总数仍 1，重复 callback 不再执行，过期 token / session / cursor 拒绝，旧 owner 不能推进 |
| 有限 rolling gate / 演练（必要） | **中**。固定 old/new binary、同 schema、dataset与冻结执行合同，先证明旧reader/writer与new共存及回退。跨 schema expand/contract 是另一个**大**扩展，不作为本轮前置 | N 与 N-1 两个真实二进制、同一 PG、实际在途未知模型 / 已落盘效果；逐副本 drain/替换 / 回滚；数据、费用和未知效果不丢；schema/capability/config mismatch拒绝转入静止升级 |
| SSO / private controller 多副本状态（未来扩展） | **大**。共享 session / nonce / revocation / key rollout 及私有调度容量，需要与原 owner、SQL quota 和身份生命周期一起完成；当前本机OIDC/private窄合同可独立成立 | 两个 host 进程 + 本地签名 IdP + SQLite/PG：跨 host callback/session/logout、同 state 一次交换、撤销跨副本即时影响下一 admission；私有同轮不能被双派发，owner/预算保持且无共享列表泄漏 |
| 跨项目统一 Studio / 全集合 SDK 分页（未来扩展） | **中**。把已有数据入口按 project 授权组合；大列表稳定 cursor、无损 revision / 错误类型；原已有项目/事件分页/调试视图不能被否认 | Rust host + 真实浏览器 + Python/TS：多角色及跨 owner 拒绝、1001+ 项遍历无重复/漏项、并发新增的 cursor 合同明确、取消 / 掉线不自动恢复工作、审批仍精确绑定 |
| SLO / 告警可执行演练 | **中**。项目 queue latency / duration / unknown / recovery 指标、低 cardinality 与无 prompt/secret、rule 测试、完整 scrape/alert 本地工具 | 真实 Prometheus/rule evaluator 对 actual host scrape；触发 waiting/failure/disabled/recovery 条件并验证时间窗、恢复和授权；recorded SLO 分母含失败与未知，未配置 destination 无外发 |
| 托管与跨区域 DR 协议 | **大**。tenant/project 生命周期、分离管理身份、版本化配置/secret references、可信全局 promote fencing、全资产 snapshot/restore；只复制五张表不包含独立 file registry / publication / rerank / 配置等资产 | 两个独立 PG + 两组真实 host/worker + 可控网络代理 / 容器拓扑：分区和单边恢复时仅一个明确 active epoch 能派发；旧区恢复仍被拒；未知费用/效果、外部对象引用、签名信任与索引 journal 完整保留；备份和恢复 receipt 有对应源码 / 配置身份 |
| 发行交付 | **小至中**。当前版本/API/feature兼容矩阵、source-only 包清单、checksum/signing/verify 工具与干净安装验收 | 从干净目录验证 Rust package、Python wheel、npm tarball 内容及许可证 / 无 secret；安装后连真实本机 host；公开 sanitized source+test receipt，实际签名由真实发行身份完成 |

私有写工具、原生流式、AWS SigV4、更多 MIME / Office / OCR、远程技能 registry 与
全自动 ontology 属于可实现的进一步扩展。它们应明确列入目标子集，不能只因为生态里
有某个插件就无限追加；也不能宣称当前已支持。当前私有 no-tools 与 offline registry
若被用户接受为本轮边界，可按此窄合同验收，不要求为了“对齐”放宽权限 / 自动上传。

## 哪些证据确实需要真实外部选择

1. 独立人类身份 / 判断、标签仲裁、未见行业材料；Agent 无法替自己生成独立真值。
2. 已支持合同的云 provider / IdP 的实际账户、区域、订阅计价、callback / TLS / key
   生命周期；需要真实身份和策略，不使用虚构成功响应替代。
3. 目标 Kubernetes / 多机 / 多区域拓扑、数据库 HA、持续负载及 SLO/RPO/RTO。
   这些具体验收环境需要用户选择；相应源码 / 演练工具仍可先在本地开发。
4. crates.io / PyPI / npm / publisher 或签名身份与公开地址。仅准备打包流程不等于
   已发行；本轮源码开源边界不能被扩展为创建收费资源或未经选择的托管服务。

“已有真实 Qwen 结果未过门槛”另列为未达标结果，不属于以上环境待选。
“源码还没有 shared session / rolling / control plane”也不属于缺环境。

## 有限 rolling 的最小设计（尚不是演练结果）

不修改 core/schema 即可做一份真实、准确的本地滚动合同：仅限两个指定二进制、
同 schema、同 SQL dataset/project、同冻结执行配置的 shared queue worker。
不承诺任意旧版、跨 schema、跨配置或新增 feature 与旧版混跑。

1. 用 `git archive 0ae4bca0b5e85ca052f419a8fe0de0560c23fac3` 导出到 Git 忽略的独占
   目录，按其 Cargo.lock 与明确 `app,database` feature 构建真实旧 taskloom；新版本
   从最终冻结工作树独立构建并复制。记录两份 source/lock/features 与实际 executable
   SHA-256，不能把同一个新 binary 复制两次或改标签当成旧/新。两者 package version
   可能都为 `0.2.0-alpha.1`，版本字符串本身不是兼容证据。
2. 本次只选旧版已有的功能合同：shared SQL queue、原 Provider/Offline、原工具与
   格式；不启用新 native/OIDC/private/Qdrant 等字段去要求旧版认得。现已只读确认旧
   commit 有 schema v3、queue/config-check/worker[-once]/inspect/diagnostics；是否兼容
   必须由实际两个程序检查。相同 schema 不是充分条件。
3. 操作员明确给出两个 binary/config/hash、owned fixture、scope、任务与上限。
   分别运行冷检查/queue读取，比较真实 schema、dataset identity、host_binding、运行
   配置、model/tool/retrieval/grants 绑定及选定能力；任何不同都拒绝。不能改旧数据、
   重新绑定队列、扩大权限或运行迁移来让 gate “通过”。凭据只经私有环境。
4. 在真实独占 PostgreSQL 下起两个旧 worker，提交有限正常任务；引入一个新 worker，
   保持至少一个已验证旧 worker 可接独立任务。记录 old/new 各自实际完成的 job、
   claim epoch 与记录，而不是只有进程存活。
5. POSIX 下逐个按真实版本支持的信号 drain worker：旧 `0ae4bca` 只有 Ctrl+C
   handler，因此用 SIGINT；当前版本支持 SIGTERM，按新合同实测。不能给旧版 SIGTERM
   后把默认硬终止称为 graceful；信号差异应进入计划/回执，不声称旧 K8s TERM 已兼容。
   当前 shutdown 走 durable pause：验收是停止
   新 claim、到达 inactive durable boundary、保留费用与未知状态、释放 lease并退出，
   不要求所有在途请求神奇地完成。可恢复的暂停任务由宿主核对后显式恢复原 ID / CAS
   revision / host binding；未知外部效果不能自动恢复。Windows TerminateProcess
   不能伪装成 graceful drain，必须使用实际 Linux/WSL 进程路径。
6. 新 worker 写出明确兼容的新记录后，启动旧 worker 并行回滚：旧 binary 读取并核对
   新的 run/session/workflow/budget/queue 记录，完成一个适合旧合同的明确任务；再 drain
   新 worker。此处必须真的执行旧 binary，不只用新 SDK parse 旧 JSON。
7. 复用真实 operational fault seed 的 unknown generation 与 fsynced effect 状态，
   在旧、新程序下分别冷读：`uncertain_attempts=1`、原 reservation、NeedsRecovery、
   tool attempts=1 与 external effect 文件=1 保持；不通过恢复未知任务来“证明可用”。
   记录五表与应用日志的允许变化，检查没有丢失、不重复相加、不静默退款。
8. schema/capability/config/grant mismatch 在隔离副本/scope上做负例，两个版本都
   在 claim/dispatch 前拒绝，实际 provider/effect 计数不增加。输出 create-only receipt，
   保留失败与各阶段 PID/绝对 executable/hash、job/epoch/预算/数据比较和清理证据。
   只操作该脚本创建的 exact processes/containers；不修改用户流量、权限、预算或现有部署。

这个闭环若实际通过，可表述为“指定 old/new、同 schema 与冻结合同下的本地 shared
worker 滚动替换和回退已验收”。schema 迁移继续明确走全部静止窗口，Helm Recreate
也不能改标签称跨 schema rolling。这一准确窄合同可以满足原“兼容策略＋演练”的
对应子项；真实生产拓扑、长期可用性与跨 schema 热迁移不随之被证明。

## 本次实际阅读的证据边界

源码 / 文档阅读包括 `rag` 的 Office、SQL/object、Qdrant、incremental；`mcp` 的
stdio / OAuth / interactions / resume；App 的 private / semantic / login / retrieval /
team / metrics；SQL schema barrier、Helm Recreate 与 SDK 方法 / 事件分页。

关键本地证据：

- `target/office-loaders-final-tests-20261128.log`：Office 9、原 DOCX 5。
- `target/source-loaders-final-tests-20261128.log`：SQLite / object 4；
  `source-loaders-final-owned-pg-tests-20261128.log`：真实 owned PG 1，容器已精确清理。
- `target/incremental-index-first-tests-20261128.log`：新增 6、原 hybrid 15。
- `target/mcp-oauth-resume-tests-20261128.log`：含当前显式 GET 7 项与 OAuth / 交互组；
  该组总 36 的范围是本地协议，不是跨厂商认证。
- `target/app-semantic-memory-sqlite-20261128-3.log` 与
  `app-semantic-memory-postgres-20261128-1.log`：显式私有语义 HTTP / SQL / 费用控制。
- `target/parity-console-reviewed-browser-20261128.json`：实际 CUA 浏览器，11 个 events、
  身份清理、显式 semantic capability/空 owner scope、0 外部模型请求。
- `target/qdrant-increment-20261128.json`：真实单机 Qdrant 两轮 SDK；
  `app-qdrant-tests-20261128-1.log` / `app-qdrant-postgres-20261128-1.log`：后续 App
  File/SQLite/PG 协议控制已存在。不能用先前 SDK-only 文档否认当前 App 源码，
  后续真实 App+Qdrant 组合与最终源码证据仍应独立写回执。
- `target/ocr-tests-20261128.log` 在读取时为 1 普通通过、2 真实容器 ignored；
  `ocr-image-build-mirror-v3-20261128.log` 已构建镜像。真实 OCR 容器结果须等实际
  ignored tests 显式执行，不因 Dockerfile / 镜像构建成功就计通过。
- `target/parity-promtool-actual-20261128-2.log` 是 TLS 下载超时，不能计 rule 实测通过。

这些 target 文件属于私有本地证据，不随源码包默认公开。最终交付需抽取不含凭据、
私有会话、数据库或原始模型内容的 sanitized receipt，绑定最终源码 / 二进制 /
配置与日志摘要。没有独立执行的检查、被忽略的外部测试和构建前的旧全量回归不能补齐
本审计列出的新功能验收。
