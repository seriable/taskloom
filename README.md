# taskloom

[![CI](https://github.com/seriable/taskloom/actions/workflows/ci.yml/badge.svg)](https://github.com/seriable/taskloom/actions/workflows/ci.yml)

**中文 | [English](README.en.md)**

一个面向 Rust 的类型化 LLM 任务与 Agent 运行时。Taskloom 将模型调用、预算、重试、审批、工具权限、持久化恢复和来源引用放进可审计的执行契约中。

> 当前版本是早期开源预览版。核心运行时和 P0–P2 的约定范围已经实现并完成有限真实环境验收；模型效果、跨领域检索、生产吞吐和生态广度仍有明确边界。请先阅读[当前完成度与限制](docs/readiness.md)和[综合验收记录](docs/acceptance/p0-p2-final-20261128.json)。

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
    let agent = Agent::new(&transport, &Environment::new());
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

## 对照结论和边界

Taskloom 在精确审批、宿主权限、预算预留、未知副作用处理、SQL 队列和恢复契约上有明确优势。LangGraph、DeerFlow、agentUniverse、CrewAI 和 LlamaIndex 在模型/数据连接器、MCP 覆盖、模板、Python 生态、托管部署、可视化调试和社区规模上更成熟。

现有同模型对照是有限实验：Taskloom 1/3、LangGraph 2/3、agentUniverse PEER 0/1、DeerFlow 0/1。提示、适配器、检索和重试条件不同，不能据此宣称全面领先，也不能把引用匹配当作事实正确性。

当前明确限制包括：

- 语言分词、reranker、Office 文档加载和大规模数据连接器需要扩展；
- MCP 的 stdio、OAuth discovery、elicitation、sampling、resource fetch、resumable SSE 和分布式会话恢复尚未实现；
- 多人服务有认证/RBAC，但 UI 仍是单操作员，私有记忆 API 尚未提供；
- 没有多机故障分区、滚动升级、自动扩缩容和生产 SLO 的完整验收；
- 当前是 Rust 核心和本地预览，不等同于成熟的托管控制面、插件注册表或 Python 多包生态。

## 验证与贡献

当前源码已在 GitHub `main` 发布。Windows 和独立 Linux 全功能回归各为 **371 通过、0 失败、17 忽略**；Rust 1.86、严格 Clippy、rustdoc、数据库/队列/容器和浏览器验收均有单独记录。托管 GitHub Actions 不是源码发布前提。

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
