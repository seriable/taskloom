# 项目运行指标

启用SQL应用后，认证读接口 `GET /api/operations` 返回队列状态计数和精确
生成配额数值、策略及版本投影；`GET /api/operations/metrics` 返回Prometheus gauges。多用户
入口相应位于 `/tenants/T/projects/P/api/...`，每次验证当前成员关系。
不提供匿名跨项目总表，不上传模型输入、工具参数、用户或凭据。
配额管理日志中的 operator 与自由文本 note 不进入这两个观测接口；完整审计
仍保留在可信管理存储和管理 CLI 中，观测读取不会删除或重写它们。

这是主动读取，默认不创建exporter/后台服务。当前基于最多1000个完整验证
的队列记录，超过边界会拒绝，不把抽样计数说成完整总数；不同记录读取
不是跨记录原子事务，不能用观测值授权预算或取代队列lease。金额和配额
在JSON中保留u64，Prometheus只用于观察，不能成为金额控制器。

`deploy/monitoring/alerts.yaml` 给出队列禁用、失败和长时间等待的示例规则。
运维方需用实际SLO审阅阈值，在Prometheus/Alertmanager中显式加载配置，
并安全配置对应项目Reader bearer。没有隐式创建告警接收人、发送消息或
收费监控实例。规则不自动批准、退款或重试任务。

## 可执行本地监控演练

`examples/operations/monitoring_drill.py` 创建独占 SQLite host、只读成员、
真实 Prometheus 容器和本机接收器，验证授权 scrape 成功、匿名 scrape
`up=0`、跨项目拒绝、队列禁用告警及恢复通知。它不提交任务，计数型本机
Provider 必须保持零请求。接收器仅实现 Alertmanager 接收协议，不代表已验证
Alertmanager 的路由、去重、静默或持久化。

```powershell
python examples/operations/monitoring_drill.py target/NEW-monitoring --taskloom target/debug/taskloom.exe --admin target/debug/examples/multiuser_host.exe --image YOUR_PRE_CACHED_PROMETHEUS_IMAGE@sha256:YOUR_EXACT_DIGEST
```

只使用预先缓存且明确指定 digest 的镜像；Docker Desktop 的默认
`host.docker.internal` 用于容器访问宿主。脚本在宿主创建临时接收端口，
完成后核对 nonce 标签、镜像 ID 并清理自己的容器和 host。生成目录含短期
只读 bearer 与数据库，必须保持私有，不复制进公开验收材料。

2026-11-28 的早期两轮各通过12项；独立审查后加上独立receiver bearer、完整
nonce标签绑定及清理修复，最终轮次13项通过，包含匿名接收者拒绝。实际
scrape、告警触发、接收与恢复均有证据，模型请求为零。实时演练仅选取QueueDisabled规则，把等待
从 5 分钟显式缩短到 2 秒；原始规则文件不变，生产 5/30 分钟条件另外由
真实 promtool 的三项虚拟时间规则测试验证。短时观测延迟不构成长期 SLO。

运行SLO应分别报告服务可达性、排队延迟、执行时间、需人工处理比例、
模型未知用量和恢复时间。模型推理受Provider控制，fixture负载的延迟不能
成为真实推理SLO。跨机器/跨区域验收仍需要相应部署环境。
