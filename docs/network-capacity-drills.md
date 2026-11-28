# 真实断网、worker 替换与容量演练

`examples/operations/network_capacity_drill.py` 在**自建且带所有权回执的
PostgreSQL fixture**上运行真实 Taskloom 进程。模型端为明确标记的本机 HTTP
协议夹具，云模型请求为 0；结果验证运行机制，不能用于模型质量排名。

## 运行门槛

- POSIX 主机，以及两份明确批准 SHA 的旧、新构建见证。脚本复用
  [同 schema 滚动门槛](same-schema-rolling.md)，校验冻结源码、真实 ELF、
  compiler、schema 3、版本与运行进程 `/proc/PID/exe`。
- 同一数据库必须已完成指定的滚动演练，并保留其未知预算/效果控制。
  网络演练没有 migration 调用，前后核对 schema、dataset identity 和未知
  namespace 的 records、leases、jobs、bindings 原始行。
- 配额管理程序来自明确校验 SHA 的旧 archive；只为本次 owned fixture 的
  `network-drill` tenant 设置独立项目。数据库连接凭据通过私有文件/进程
  环境传入，不能放入公开命令、日志或开源包。

调用需要提供 `--old-report` / `--old-report-sha`、`--new-report` /
`--new-report-sha`、`--admin` / `--admin-sha`、`--postgres-fixture` 与
`--rolling-report`，并指定尚不存在的独立输出目录。所有权、来源或上轮
结果校验失败时必须终止；不能靠修改 lease epoch 或预算行让演练通过。

## 实际 1 → 2 → 1 与替换

1. 一个 worker 持有任务 A 的真实 SQL lease 和已发出的 HTTP 请求，任务 B
   留在队列中；启动第二个不同 PID，等待它实际认领 B 并发出请求。
2. 释放 A 的已知结果，确认完成后向空闲的第一个 worker 发送 SIGTERM。
   必须退出码 0；强杀清理不算 graceful drain。
3. 留存第二个 worker，释放 B 并再提交 14 个任务。共 16 个健康任务必须
   全部 completed，各恰好一次 claim 和一次对应 HTTP 请求。

排队、执行和总时长来自全部任务的数据库事件：enqueue→claim、
claim→finish、enqueue→finish。任何失败、缺失或时间顺序异常都会使整轮
失败，不能删除慢样本或只统计成功子集。报告保留 `expected_jobs`、
`completed`、`failed`、HTTP 总数与每个分布的 `samples`。

## 真正切断连接与保守恢复

`OwnedTcpProxy` 仅转发到固定 loopback PostgreSQL 端口，不记录 SQL、
凭据或数据包内容。隔离动作会关闭已有客户端/上游 socket，并拒绝新连接；
恢复只允许新建连接，不会复活旧连接。这是实际网络 I/O 故障。

- 故障项目只允许一个 outstanding attempt。原 worker 已持久化预算与项目
  配额预留、并发出请求后，关闭真实 DB 连接，再释放之前已发出的夹具结果。
- 控制端通过直接 owned DB 连接提交 backlog；经过隔离代理的新 worker
  必须失败且不能发出模型请求。原 owner 必须非零退出并保留未知状态。
- 等待数据库时钟上的 lease 真正过期，恢复网络，启动一个 successor。
  记录数据库实际分配的新 owner 和更高 epoch，**不注入 epoch**。
- 一个短暂 `SELECT … FOR UPDATE` 配额行锁仅让很快结束的 successor lease
  有机会被观测，不构成网络故障，也不更改 owner、epoch 或预算。
- successor 必须保留原 run/job 的 unknown attempt；原零价格调用依旧
  消耗次数预算，项目 uncertain attempt 为 1，in-flight 为 0。原 job 在
  第二次认领后安全 stopped，backlog 从未认领，故障请求总数仍为 1。

这轮恢复的分母是 **1 个恢复到保留预算安全停止的样本**，预期成功任务
完成数为 **0**。它不能描述为任务自动恢复成功或业务可用性 SLO。未知工具
效果控制仍只 cold-read，外部 append 文件仍恰好一行；不会自动重放或退款。

## 已有验收与边界

2026-11-28 首轮本机验收 7/7 顶层检查通过：健康任务 16/16、HTTP 16 次，
另一个故障 HTTP 1 次；实际关闭 4 条连接并拒绝 6 次新连接。原 owner
epoch 1，successor epoch 2；断网到安全停止 12023 ms，网络恢复到安全停止
2112 ms。这些是有显式响应门控、真实 SQL 与进程调度的有限样本。

该历史轮次绑定的 ELF SHA 为
`73848f49be9305d2c56b115c0d9f85ef3d62b0aac36b9ca5ec37682db2b4d6bf`，
是 durable MCP 增量前冻结的候选。后续最终源码/ELF 必须单独建立见证并重跑，
不能把该结果转贴为新二进制已通过。

durable MCP 与文档多样性增量冻结后，已对最终 ELF
`8e996d20152f57f6c134f7ffc463555c2bdb12b1fb00c2d728231786f532dcf2`
建立独立源码见证、完成前置滚动矩阵并重新运行本演练：7/7 检查通过，
16/16 健康任务、16 次 HTTP，故障 HTTP 仍为 1 次、恢复后额外请求为 0。
本轮实际切断 3 条连接并拒绝 6 次新连接，epoch 1→2；断网到安全停止
12463 ms，恢复到安全停止 2418 ms。排队/执行/总时长的分母均完整保留 16。
报告位于 ignored `target/network-capacity-final-20261128-1/`；unknown
预算/效果未退款或重放，仍只计 1 个安全停止样本、0 个成功恢复完成任务。
全部 owned 进程/代理已停止；指定 PostgreSQL 容器及匿名数据卷已按
ID、nonce label、image 校验后删除，未触碰其他 fixture。

这是单台物理主机上的真实进程、SQLite/PG 滚动前置和 PostgreSQL TCP 故障
演练；不证明多主机分区、数据库 HA、Kubernetes HPA、跨 schema 升级、长期
负载容量或生产 SLO。结果与逐项 SQL/进程日志留在本地 ignored `target/`；
输出目录和容器只能按本次所有权回执清理。
