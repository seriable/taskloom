# 冻结评测、人工标注与严格可比条件

本轮提供评测基础设施，**没有运行模型、竞品或新 embedding 请求**。
它不把已有失败改成通过，也不把 Agent 编写的标签称作独立人工标注。

## 跨语种与跨领域开发集

`examples/benchmarks/parity-evaluation-v1.json` 固定八份资料和 28 条查询：
六份已经见过的仓库完整技术文档，加两份明确虚构的中英双语财务审核、
预约政策。虚构文本只用于区分领域，不是财务或医疗建议。

查询有可重叠的 `chinese`、`english`、`mixed`、`short`、`paraphrase`、
`negative`、`identifier`、`cross_domain` 分组。`label_author` 明确写明
维护者 Agent 编写，尚无外部人工审核。该集不是 held-out、盲测或大规模
独立基准。技术来源为已有完整文档，不能据两份虚构政策宣称覆盖行业知识。

每份仓库文档有 UTF-8/LF 哈希和完整正文绑定；来源漂移会在准备阶段拒绝。
结果必须保留每组原始分数、空结果、错误、缺失案例，不从分母删除。初次
运行前应单独冻结数值门槛、模型/配置和费用上限；当前文件不假装已有通过
结果或预先宣布产品验收。

```powershell
python examples/benchmarks/parity_evaluation.py dataset --suite examples/benchmarks/parity-evaluation-v1.json --out target/NEW-parity-dataset.json
```

输出可直接作为 `native_retrieval.py --dataset` 的输入；该原生检索脚本
仍需显式本地模型服务、凭证环境和预算。本命令只准备数据，没有启动服务。
用现有固定 profile 运行后，按组重新评分：

```powershell
python examples/benchmarks/parity_evaluation.py score-retrieval --suite examples/benchmarks/parity-evaluation-v1.json --report target/ACTUAL-CAMPAIGN/report.json --phase hybrid --out target/NEW-parity-retrieval-scores.json
```

`model-inputs` 命令仅导出 `{id,query}` 的允许字段，不包含 relevant、
预期标签、作者或评审内容。评测标签留在宿主评测端；不得把完整带答案的
数据集直接作为模型请求。本工具不提供调用模型的网络路径。

## 外部独立人工标注格式

人工标注文件需要由真实外部评审者填写。下面是**格式模板，不能直接当作
已有人工评价**。使用真实标识、与项目/输出的关系及独立性声明，绑定每个
原始答案的 SHA-256；不要由编码 Agent 填造人名和人类身份。

```json
{
  "version": 1,
  "kind": "external_human_annotations",
  "author": {
    "id": "ACTUAL-EXTERNAL-REVIEWER-ID",
    "kind": "human",
    "independent": true,
    "affiliation": "ACTUAL-AFFILIATION-AND-RELATIONSHIP",
    "declaration": "ACTUAL-INDEPENDENCE-DECLARATION"
  },
  "cases": [{
    "id": "ACTUAL-CASE-ID",
    "author": "ACTUAL-EXTERNAL-REVIEWER-ID",
    "output_sha256": "SHA256-OF-EXACT-ORIGINAL-ANSWER-UTF8",
    "conflict": {"status":"none", "note":""},
    "coverage": {
      "exact_provenance": null,
      "source_support": null,
      "corpus_factuality": null,
      "task_coverage": null
    },
    "reason": "Source-relative reasoning and references supplied by the actual reviewer."
  }]
}
```

`null` 明确未评价，在评分中不通过。每条必须绑定作者和四个维度；冲突状态
为 `none`、`open` 或 `resolved`，`open` 保留在分母且不计通过。多个评审者
的意见不能未经记录自动合成“多数真理”；仲裁后的文件需保留冲突说明及原
文件，或分别公开各人的原始评分。脚本只校验格式，不核实人类身份和独立性，
报告始终写 `human_identity_verified:false`、`independence_verified:false`。

维度分别定义，不互相替代：

| 维度 | 标签 | 通过条件 |
| --- | --- | --- |
| 原文/证据来源 | `verified/failed/missing/error` | `verified` |
| 附引文是否支持原观点 | `supported/contradicted/insufficient/error` | `supported` |
| 完整语料是否支持原观点 | `supported/contradicted/insufficient/error` | `supported` |
| 是否覆盖原任务要求 | `complete/partial/missing/error` | `complete` |

完整语料事实性仍是**来源相对**的评价，不等于现实世界客观真理。原文匹配
不能推出观点被支持，观点被支持也不能推出遗漏的任务要求已完成。

```powershell
python examples/benchmarks/parity_evaluation.py score-answers --expected target/expected-case-ids.json --outputs target/actual-original-outputs.json --annotations target/actual-external-human-annotations.json --out target/NEW-separated-scores.json
```

`expected` 是事先冻结的案例 ID 有序数组，`outputs` 是原始 `{id,text}` 或
`{id,error}` 数组。答案被修改或引用了不同答案哈希时直接拒绝。输出缺失、
执行错误、未标注、未解决冲突保留在每一维度分母中；不提供合成“总正确率”。

## 多人标注一致性（方法验收）

对同一冻结 case 列表的 2–16 份不同作者标注，可分别计算四个维度的
pairwise nominal Cohen kappa：

```powershell
python examples/benchmarks/parity_evaluation.py agreement --expected target/expected-case-ids.json --annotations target/actual-human-a.json --annotations target/actual-human-b.json --out target/NEW-agreement.json
```

作者重复、同 case 原答案哈希不同会拒绝，不能把改写后的答案当作分歧。
缺失标注、缺维度、annotation error、open conflict 不作为一致类别，分别报告
`excluded`、完整 `expected_cases`、`paired_valid` 和 `coverage`；不会因为两人
都没标注而得到高一致率。Kappa 的数学样本只包含成对有效标注，与完整覆盖率
一起呈现；零有效样本或完全常数边际时为 null 并给原因，不伪造为 1。

这不是正确率，也不会生成共识标签、代替冲突裁决或验证作者身份/独立性。
测试使用显式非真实人的 schema fixture，当前没有真实多人标注验收。

## 框架条件字段

每个实际运行框架保存版本 1 manifest，含 `framework` 的 `name`、`version`、
`source_identity`，及完整 `conditions`：

- `model`：provider、endpoint_identity、model、完整显式 parameters。
- 有序 `tasks`，及 `corpus_sha256`、`system_prompts_sha256`。
- `tool_schema_sha256`、`tool_grants_sha256`、`output_schema_sha256`、
  `retrieval_policy_sha256`。
- `output_tokens`、`max_http_attempts_per_case`、`deadline_seconds`。
- `per_case_budget`：currency、max_cost_micros、prices_sha256。
- 完整 `retry_policy`，包括输出修复和上游重试的实际规则。

模型名相同不够：推理开关、采样参数、真实端点、原任务/输出 schema、总
HTTP 尝试上限、预算、价格、权限、检索和重试都必须显式。参数不能靠隐式
默认值填补，也不能把保护上限不同的两个运行称作严格同预算比较。

```powershell
python examples/benchmarks/parity_evaluation.py compare --reference target/taskloom-actual-manifest.json --candidate target/other-actual-manifest.json --out target/NEW-comparability.json
```

任何字段不同返回 `not_comparable` 和精确字段名。全部相同只返回
`declared_conditions_equal`，**不会自动准许性能排名**：声明还需与实际原生
请求、流量日志、适配器控制代码、完整运行结果和未知用量核对。仅 import
框架、生成模板清单或运行本离线比较器不等于执行过该框架。各框架原生提示、
工具协议或调度差异不能隐藏；确实不兼容时保留原因，另列合格但非严格可比
的观察，不强行评分。

输出文件均为 create-only，防止覆盖旧验收。离线控制测试：

```powershell
python tests/parity_evaluation.py
```

本轮接受的是准备、隔离、分母和可比性校验方法。真实跨语种模型效果、外部
人工标注、同模型严格条件竞品运行和统计置信度仍需后续实测，不由类型或
单元测试自动宣告完成。

## 从实际运行核对条件

`langgraph_compare.py` 在实际构建原生StateGraph时保存所采用的模型、工具schema、
grants、审批、步数/工具/费用/字节限制和context策略；安装的LangGraph公开源码
文件另记录哈希。它在派发前拒绝需要原context compressor的运行；本适配器只
支持无压缩历史。显式零价loopback API、无环境代理/redirect、有界响应和未知
用量停止后续案例也是该适配器的限制，不能据此声称所有LangGraph应用等价。

```powershell
python examples/benchmarks/langgraph_compare.py target/ACTUAL-taskloom target/NEW-langgraph --config target/explicit-zero-fee-local-model.json --taskloom target/debug/taskloom.exe --verifier target/debug/examples/benchmark_citations.exe
python examples/benchmarks/compare_parity.py target/ACTUAL-taskloom target/NEW-langgraph target/NEW-observed-parity.json --require-applied-contract
python tests/observed_parity.py
```

实际比较器检查有序完整案例分母、每一步真实请求、仅归一化opaque call IDs、
原始结果/用量与attempt记录、实际采用的权限/限制合同，并绑定原始记录哈希。
旧记录缺少applied contract时保持未证明，不补写字段让旧运行通过新gate。
四项独立控制涵盖漏案例、相同请求但grants不同、缺少旧合同证据和create-only。

2026-11-28的候选Taskloom和原生LangGraph1.2.24，使用真实本机Qwen3-1.7B、
原5个算术案例、同模型/提示/工具/1024输出上限/显式零API价格：两者均2通过、
3失败、0未执行。实际请求上下文、尝试计数及applied contract一致。失败保留，
这证明该小型原生adapter对照已执行，不能推出引用质量、长任务、崩溃恢复、
速度或通用框架排名，也不把DeerFlow/agentUniverse的不同原生产品合同算作相同。

## 本机实验性 learned reranker

`examples/local_models/serve_reranker.py` 显式使用已经锁定的
Qwen3-4B-Instruct-2507权重，对query/passage做一次prefill并计算yes/no末token
相对logit分数。它没有生成答案、读取gold/标签或访问外部服务；模型文件、
实现、提示、阈值、batch、数值精度/设备和依赖版本绑定服务身份。它不是厂商
专用reranker，也不是经过校准的正确率、独立judge或事实真值。

运行前用[本机模型下载工具](../examples/local_models/download.py)的
`generation-4b-instruct` profile准备并核对公开权重；安装torch/transformers
和选择硬件由宿主显式完成，源码包不包含模型文件。

```powershell
python examples/local_models/serve_reranker.py target/local-models-4b-instruct --port 18094 --key-env TASKLOOM_LOCAL_RERANK_KEY --max-requests 128 --min-score 0.5 --observations target/NEW-private-hashed-scores
python examples/benchmarks/native_retrieval.py target/NEW-rerank-campaign --taskloom target/debug/taskloom.exe --model-root target/local-models --dataset target/frozen-development-dataset.json --embedding-url http://127.0.0.1:18093/v1 --profile target/frozen-profile.json --protocol target/frozen-protocol.json --rerank-service target/explicit-zero-fee-local-rerank.json
```

`--rerank-service` 是显式AppHttpRerank配置，本例额外在任何派发前拒绝非loopback
或非USD零价格配置；生成/embedding/rerank账本仍独立。可选observations目录仅保存
query/document哈希、分数和实际输入token，不保存正文/凭据；哈希仍可能关联私人
内容，因此该目录和数据库/ledger都须保持私有。源码开源不会启动该服务。

首轮真实开发实验：29次embedding和51次rerank（755 pairs），召回17/21、
MRR@5为0.762、负例7/7、0错误；整体门槛仍失败。它减少了负例误召回，同时
损失正例，不能被描述为效果已经补齐。原两轮失败和原profile/阈值保留；
新实验使用新的冻结配置与回执，也不把可见复用开发集称为未见测试。

后续明确使用前一失败实验的可见分数校准阈值为`0.0001`，并启用
`max_chunks_per_document:1`，避免同一文档的多个chunk挤占全部结果位；原标签
与通过门槛不变。第三轮真实实验召回21/21、负例7/7、MRR@5为0.929、0错误，
通过该**可见开发集**门槛。5项服务拒绝/零推理控制也通过，51次rerank及29次
embedding各自记账；先前所有失败保留。这不是独立未见样本或总体效果认证。

可复现的精确profile为
[`retrieval-profile-qwen-answerability-v1.json`](../examples/benchmarks/retrieval-profile-qwen-answerability-v1.json)，
对应[原门槛与新复现元数据](../examples/benchmarks/parity-retrieval-answerability-v1-protocol.json)
另记录实际被测protocol的摘要。将上方服务命令的`--min-score`显式设为
`0.0001`，并传入这两个文件；实际模型identity从该服务`/health`读取，再由
宿主填入独立AppHttpRerank配置，不能把旧阈值的ledger改名复用。

模型目录必须与manifest的文件集合完全一致；额外模板/adapter目录、间接文件
或任何锁定字节变化在加载前拒绝，防止未绑定`chat_template.jinja`改变评分。

通用`PerDocumentRetriever(inner, max_per_document, candidates)`先验证完整候选
集合再保留每文档上限与原顺序/分数，最后取top-k；不改来源、不自动补语料。
App默认`max_chunks_per_document:0`且序列化省略，旧身份不变。非零设置绑定
retriever/tool/queue/run合同，并在外部rerank已授权候选数不足时静态拒绝；
原model/corpus未变的embedding向量可以复用，不能把该设置理解为强制重嵌入。
