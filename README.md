# taskloom

[![CI](https://github.com/seriable/taskloom/actions/workflows/ci.yml/badge.svg)](https://github.com/seriable/taskloom/actions/workflows/ci.yml)

**中文 | [English](README.en.md)**

一个面向 Rust 的类型化 LLM 任务与 Agent 运行时。Taskloom 将模型调用、预算、重试、审批、工具权限、持久化恢复和来源引用放进可审计的执行契约中。

> 当前版本是早期开源预览版。本轮 P0–P2 的代码与本地验收已有[公开摘要](docs/acceptance/framework-parity-local-20261128.json)：Windows 510/Linux 512 项通过，实际容器/数据库/滚动回退/受管灾备分别验收。真实云端、独立人工与未见资料、生产长期 SLO 仍待对应环境；完整范围以[逐项进度与边界](docs/framework-parity-roadmap.md)为准，不能据此宣称全面领先。

## 快速开始

```toml
[dependencies]
taskloom = { git = "https://github.com/seriable/taskloom", branch = "main", features = ["openai"] }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

```rust
use std::sync::Arc;
use taskloom::openai::OpenAiCompatible;
use taskloom::{json_value, Agent, Environment, Gateway, GatewayTransport, PromptTask, Protocol, TaskSpec};

#[tokio::main]
async fn main() -> Result<(), taskloom::TaskError> {
    let gateway = Arc::new(Gateway::default());
    let provider = OpenAiCompatible::new(
        "https://api.openai.com/v1",
        std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY is required"),
    );
    let transport = GatewayTransport::new(gateway, provider);
    let environment = Environment::new();
    let agent = Agent::new(&transport, &environment);
    let task = PromptTask::new(
        "summary",
        TaskSpec::new("gpt-4o-mini", Protocol::ResilientJson, 0.2, 400),
        r#"Summarize in one sentence. Answer {"summary":"..."}."#,
        "Taskloom runs typed LLM tasks with fallbacks and replay.",
        json_value,
    );
    println!("{}", agent.run(&task).await?);
    Ok(())
}
```

要求 Rust **1.86 或更高版本**。想运行本地 CLI/API/UI，可执行：

```bash
cargo install --path . --locked --features app
taskloom init taskloom.json
```

默认 `features = []`，因此只使用类型化任务的用户不需要引入运行时、数据库或应用依赖。已有 `AgentTask` API 保持兼容。

## 能力概览

| 能力 | 说明 |
| --- | --- |
| 类型化任务 | `AgentTask` / `PromptTask`，结构化输出校验、模型链和可靠 JSON 协议。 |
| 网关与预算 | 超预算请求在发送前拒绝；支持超时、取消、熔断、重试、降级、账本和 Provider 用量。 |
| Agent 运行时 | 工具调用、Schema 校验、显式权限、执行事件、模型/工具/时间上限。 |
| 持久化协作 | 检查点、精确输入审批、会话、上下文压缩、并行/条件 DAG、PEER 评审返工。 |
| 安全恢复 | 预算先于派发持久化；未知用量和未知外部效果保留，不静默重放。外部服务不参与事务，因此不承诺通用 exactly-once。 |
| 研究与引用 | 文档导入、网页/PDF、lexical/embedding/hybrid 检索、来源定位、引用原文校验。 |
| 长期记忆 | 有来源候选、人工审核、冲突更新、编辑、遗忘和作用域隔离；模型不能授予权限。 |
| 技能与连接器 | 版本化数据包、依赖/凭证名声明、宿主授权、MCP 示例和可恢复外部写入。 |
| 观测与产品 | 父子 Trace、默认关闭的 OTLP、诊断/评测视图、CLI/API/UI 工作流编辑和运行恢复。 |
| 服务化 | SQLite/PostgreSQL、身份/角色、项目隔离、配额、事务队列、跨进程 worker、lease fencing。 |

完整功能和配置入口见：

- [运行时指南](docs/agent-runtime.md) · [持久化运行](docs/durable-runs.md) · [工作流](docs/workflows.md)
- [检索与研究](docs/knowledge.md) · [长期记忆](docs/long-term-memory.md) · [技能包](docs/skill-packages.md)
- [MCP 集成](docs/mcp.md) · [应用/API/UI](docs/application.md) · [SQL 存储](docs/sql-storage.md)
- [P0–P2 开发清单](docs/p0-p2-roadmap.md) · [框架对照](docs/reviews/framework-comparison-20261128.md)
- 本轮扩展：[原生 Provider](docs/native-providers.md) · [检索与 Rerank](docs/retrieval-extensions.md) · [私有会话](docs/private-sessions.md) · [OIDC 登录](docs/login-service.md) · [部署](docs/production-deployment.md) · [Python/TypeScript SDK](docs/client-sdks.md)

## 对照结论和边界

Taskloom 的重点是精确审批、宿主权限、预算预留、未知副作用处理、SQL 队列和恢复契约。LangGraph、DeerFlow、agentUniverse、CrewAI 和 LlamaIndex 在模型/数据连接器、模板、Python 生态、托管部署、可视化调试和社区规模上更成熟。

现有同模型对照是有限实验：Taskloom 1/3、LangGraph 2/3、agentUniverse PEER 0/1、DeerFlow 0/1。提示、适配器、检索和重试条件不同，不能据此宣称全面领先，也不能把引用匹配当作事实正确性。

当前明确限制包括：

- 已有显式reranker、DOCX/XLSX/PPTX、CSV/JSONL/UTF-8邮件、SQL/对象快照、Qdrant和受限英中OCR；格式子集、费用和来源边界见[检索扩展](docs/retrieval-extensions.md)，不能推导广泛语料效果；
- MCP 已有stdio/resources、宿主审核回调、固定OAuth/PKCE和显式SSE GET；独立[SQL持久恢复SDK](docs/mcp-durable-recovery.md)只恢复原session，拒绝该模式的callbacks，不自动重POST或核销未知工具效果；
- 已提供可选多用户工作台、私有记忆/会话和 OIDC/PKCE 本地验证；真实外部 IdP、多副本私有调度仍待验收；
- 指定旧新二进制的[同schema滚动回退](docs/same-schema-rolling.md)、本地故障与[实际监控](docs/operational-metrics.md)分别有受限演练；真实多机分区、跨schema热升级、自动扩缩容和生产长期SLO仍不能由此推出；
- 当前是 Rust 核心和本地预览，不等同于成熟的托管控制面、插件注册表或 Python 多包生态。

源码发行的[兼容矩阵](docs/source-compatibility-matrix.md)和[checksum/签名验证准备](docs/source-release-preparation.md)
单独记录发行边界；离线测试密钥不会作为维护者身份发布。

## 验证与贡献

本轮完整回归为 Windows **510通过/0失败/31忽略**、Linux **512通过/0失败/31忽略**；两平台最小 feature 各84通过。31个ignored不计通过，真实PostgreSQL/OCR等按独立回执验收。Rust1.86编译、严格Clippy、rustdoc、数据库/队列/容器和浏览器分别验证；[公开摘要](docs/acceptance/framework-parity-local-20261128.json)记录原始证据摘要与边界。托管GitHub Actions不是源码发布前提。

运行测试：

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-features
cargo doc --locked --no-deps --all-features
```

请阅读[贡献指南](CONTRIBUTING.md)。不要提交 API 密钥、环境文件、录制数据、模型权重或本地缓存。英文完整说明见 [README.en.md](README.en.md)。

## 许可证

MIT 或 Apache-2.0，详见仓库中的 `LICENSE-MIT` 和 `LICENSE-APACHE`。
