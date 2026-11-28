# SQL 队列、进程故障与静止备份演练

此工具包验证同一台机器上的真实 SQLite、多进程队列和独占 Docker
PostgreSQL。模型决策使用明确标注的离线夹具；工具副作用是实际写入并
`fsync` 的本地文件。它补充运行时契约测试，不代表模型效果、多机网络分区、
生产容量、长期 SLO 或滚动 schema 升级已验收。

## 准备与执行

要求 Python 3、Rust 1.86+、Docker Linux engine，以及本地已有的固定
PostgreSQL 官方镜像。`postgres_fixture.py` 不拉取镜像、不调整 Docker
daemon/代理，也不接管用户已有数据库。若没有缓存镜像，需要先单独准备
脚本中注明的官方 digest。

在项目根目录执行，输出目录每次使用新名称：

```powershell
cargo build --locked --features app,database --bin taskloom --example multiuser_host
cargo test --locked --features database --test operational_faults --no-run
python examples/database/postgres_fixture.py create target/fault-pg-01
# 使用上一条 cargo 输出的 operational_faults 可执行文件实际路径。
python examples/operations/fault_drill.py target/fault-drill-01 `
  --taskloom target/debug/taskloom.exe `
  --admin target/debug/examples/multiuser_host.exe `
  --fault-test target/debug/deps/operational_faults-<hash>.exe `
  --postgres-fixture target/fault-pg-01
python examples/database/postgres_fixture.py remove target/fault-pg-01
```

Linux 使用相应无 `.exe` 的路径。编译开启 app/database 所需 feature；
保留 `--locked`。也可以使用已构建的 all-feature 二进制。脚本在运行前复制
三个可执行文件，避免其他编译替换正在使用的文件。调用方应在成功或失败后
都执行最后的 owned fixture 清理命令；工具自身始终终止自己的服务进程、
解除自己发起的数据库暂停并删除自己创建的恢复数据库。

单独运行无需 Docker 的 SQLite 进程故障控制：

```bash
cargo test --locked --features database --test operational_faults sqlite_operational_process_and_owner_faults -- --nocapture
```

未指定 `TASKLOOM_FAULT_OUTPUT` 时测试使用自动清理的临时目录。需要保留
逐项回执时，先创建一个全新的输出父目录，再通过该环境变量传入其绝对路径；
测试在下面创建 `sqlite/` 或 `postgres/`，拒绝覆盖已有 backend 目录。
PostgreSQL 单独测试是显式 ignored 测试，仅接受调用方通过
`TASKLOOM_TEST_POSTGRES_URL` 环境变量提供的独占测试库。不能指向生产库。
`fault_drill.py` 只接受 owned fixture 目录，读取私有凭证后经环境传入子进程。

## 验证内容

| 场景 | 真实触发与通过条件 |
| --- | --- |
| 有限积压 | 每个后端 4 个提交线程、4 个实际 worker 进程、48 个正常任务；开放队列/并发上限拒绝、跨项目隔离、实际 API 重启及显式恢复控制；引用 `queue_load.py` 的逐项结果。 |
| 模型派发期间进程死亡 | 夹具在持久化预算预留后写入同步标记并停住；父进程强制终止确切子进程。继任者等待真实 DB lease 到期，保留零价格未知调用次数；项目调用次数上限阻止再次派发。 |
| 非幂等效果回复丢失 | 精确输入审批后，子进程实际追加并同步文件，在返回回复前被终止；继任者进入 `NeedsRecovery`，反复恢复也不重新调用模型或工具。 |
| 人工对账与待恢复状态 | 一组实际读取效果文件后显式记录结果，再完成任务；另一组故意不对账，保留 `NeedsRecovery` 与原 call、attempt=1，交给静止备份/恢复验证。 |
| owner fencing | 显式本地 SQL 修改 queue owner/epoch 并使其过期；旧 owner 不能通过派发验证、写 checkpoint 或覆盖预算日志，活跃 runtime lease 仍阻止过早接管。此项是故障注入，不是实际网络分区。 |
| SQLite 写锁 | 实际 `BEGIN IMMEDIATE` 锁跨越完整 CLI admission 与 busy timeout；拒绝后所有表记录一致。释放锁后由操作者显式重交同一任务，验证只完成一次。WAL 读操作不要求报错。 |
| PostgreSQL 暂停 | 校验 container ID、随机 ownership label 和 image ID 后，只暂停该独占容器。正式 host `/health/ready` 返回 503，`/health/live` 仍 200；解除暂停后恢复 ready，任务/预算记录不变。此时不派发模型或工具。 |
| 静止备份/恢复 | 停止所有 owned worker/host，并以数据库时钟确认无活跃 lease。SQLite 使用原生 backup；PostgreSQL 使用 `pg_dump -Fc` 与新库单事务 restore。逐行比较五张持久化表，再执行无派发的 queue/quota 冷检查。 |

备份比较范围包括 `taskloom_meta`、`taskloom_records`、
`taskloom_leases`、`taskloom_jobs`、`taskloom_job_bindings`，连同 namespace、
family、object ID、revision、body、排队索引和绑定。未知模型预算/项目调用次数
以及未对账工具状态在恢复后仍然存在；不会把 checkpoint 和预算 journal
重复相加，也不会用丢失回复推断效果未发生。

## 机器可读回执与数据边界

总回执为输出目录的 `report.json`，包含 `completed`、`status`、逐项布尔
`checks`、`failures`、复制后二进制与源码文件 SHA-256、日志 SHA-256、
本地 PostgreSQL digest/配置和测量边界。子回执在 `load/report.json`、
`faults/{sqlite,postgres}/report.json`；失败保留原始分母与失败日志，不覆盖
前次运行。源码指纹是运行时采集值，二进制 digest 单独记录；这不能替代最终
源码冻结后的可复现构建证明。

输出中数据库、dump、bearer 文件和完整运行记录都是私有测试数据，只放在
被 Git 忽略的 `target/`。不要将整个目录作为开源制品发布。PostgreSQL URL
只经子进程环境传递，日志对私有 URL/密码脱敏；清理仍会核对 ownership，
不会按名称模糊匹配或删除其他容器。

本地效果文件不属于数据库备份。恢复数据库既不能撤销已发生的外部效果，
也不能证明外部效果已经失败；必须保留未知状态并显式核查。

## 升级与回滚策略

此包没有验证不同版本之间的滚动 schema 升级。当前安全升级流程是：停止
admission、暂停/清空调度、等待或人工处理运行中与未知效果任务、停止所有
worker/host、确认无活跃 lease、备份并验证新目标恢复，再执行显式迁移和
冷启动检查。需要保留旧二进制、配置、数据库备份及外部效果审计记录。

只有已证明 schema、宿主冻结身份和序列化执行契约兼容的版本才适合逐副本
替换；更改宿主契约会拒绝恢复旧运行，应先处理旧运行再升级。恢复旧数据库
必须同步核查已产生的外部效果，不能把数据库回滚当作外部系统回滚。本地
同版本进程重启和静止恢复不作为跨版本滚动升级验收。

真正对齐生产运维仍需在目标环境补做多机/真实网络分区、数据库 HA/主从切换、
版本兼容矩阵、受控扩缩容、实际 Kubernetes 集群、长时间告警/SLO 与灾备演练。
