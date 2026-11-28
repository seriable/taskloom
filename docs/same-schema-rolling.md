# 同 schema 的 worker 替换与回滚

本策略只允许**实际冻结执行契约相同**的 worker 在同一 SQL dataset 上短暂
共存。schema、配置、工具/角色能力、身份或序列化状态不兼容时，必须拒绝
滚动，转静止窗口处理；不会改写 hash 使检查通过。本地演练不修改 runtime、
数据库 schema 实现、真实 admission 或生产权限。

它不等于通用跨版本升级。当前已经测试的旧源码来自精确 Git commit
`0ae4bca0b5e85ca052f419a8fe0de0560c23fac3`，新源码是单独记录 SHA 的
开发快照。两者 `--version` 都为 `0.2.0-alpha.1`，因此版本号本身没有证明力。
旧 source archive、Cargo.lock、编译器、feature 集、两份不同的实际 ELF
SHA、源文件见证和每个 live PID 的 `/proc/PID/exe` 都单独记录。

## 必须先通过的门槛

1. 宿主明确批准两份 build witness 的 SHA；实际旧 archive/lock、当前冻结
   源字节、ELF、compiler/schema/version 都与见证一致。相同二进制贴两个
   标签不算 old/new 演练。见证记录由宿主提供的构建来源，不能冒称仅凭
   两个 hash 就证明了可复现构建。
2. 两个真实 CLI/API 在同一个 fresh owned 数据库读取 schema **3** 与原
   dataset identity；实际计算的 queue frozen binding 必须完全相同。
   同时比较 active tool catalog 和 role contract，限定共同的执行 profile。
3. 新编译包可以具有更多可选 features，但不能启用旧 worker 不支持的
   private/native/OIDC/Qdrant/其他新能力。本轮 profile 是固定 loopback
   OpenAI-compatible 协议夹具、明确零价、writer 文本角色；实际 gate 回执
   的 active tool catalog 为 `[]`，不会仅凭配置猜测“无工具”。
4. 主 fixture 的 schema/dataset 保持不变。fresh schema 初始化和独立 SDK
   unknown fixture 的同 schema 初始化只在门槛前执行；通过 gate 后没有
   migration 调用。schema4、不同 dataset、模型/工具配置的拒绝控制只作用于
   独立复制库/配置，不能回写主库。

## 替换与回滚步骤

本地脚本 `examples/operations/rolling_rehearsal.py` 只在 POSIX 运行。
旧 `0ae4bca` 的 worker 只处理 `SIGINT`/Ctrl-C，尚不支持 SIGTERM drain；
新 worker 同时支持 SIGTERM。本轮分别发送真实 SIGINT 与 SIGTERM，
Windows `TerminateProcess` 不计作 graceful drain，也没有声称旧版本已经
适配 Kubernetes 默认 SIGTERM。

- 启动两个旧 worker，分别在实际持久化 queue/run lease 和已发出的本机
  HTTP 夹具请求处等待；加入第三个新 worker，记录三个 PID/ELF/lease
  同时存在，确认并发来自不同源码二进制。
- 先释放新 worker 并让它完成正常 probe，保持可服务的候选。逐个释放旧
  worker 的**已知响应**，等待该任务确实 completed、没有待抢任务，再向
  该空闲 worker 发 SIGINT。要求退出码 0、无新 claim；不向付费 in-flight
  请求强发 cancel 来追求“顺利”退出。
- 新 worker 独立完成新的 run、session turn 和 workflow/child；旧 CLI/API
  精确冷读新写入的状态和 budget。旧 worker 回滚加入，完成由新端提交的
  probe；新 worker 结束已知任务后 SIGTERM 退出，旧端继续同一 session
  的第二 turn、另一个 workflow 和正常任务。
- 每后端 13 个健康逻辑任务各 1 次 claim、各 1 次标记明确的模型 HTTP
  请求。分母包含全部任务与全部 fixture 请求，检查旧 terminal run/budget
  未丢失/改变，未用重新创建任务替代恢复。

故障或未知状态不能自动归入成功：若主 profile 出现未完成/unknown，脚本
停止并保留 failure SQL/log；不会硬清账、退款、修改审批或自动 resume。
这不是 in-flight cancellation 的安全性承诺。

## 未知预算与效果的保留

脚本复用当前 `operational_faults` **真实测试 ELF**，在同一个数据库的
独立 namespace 中执行真实进程死亡、持久化预算和 fsynced 效果控制。
这不是把当前 SDK 编译两遍冒充 old/new runtime。

实际旧、新 CLI 都只 cold-inspect 这些 fixture：未知零价格调用次数仍为
1，未对账效果仍为 `NeedsRecovery`/原 call/attempt=1，外部文件始终只有
一次 append。整段 rolling/rollback 前后，该 tenant 的 records、leases、
jobs 和 bindings 原始行都完全一致，包括 quota/journal；不会将 checkpoint
和预算 journal 重复相加。未知 namespace 从不交给正常 worker resume。

拒绝控制另验证两份真实 worker 对模型/能力配置变化拒绝 claim、拒绝
schema4 clone、拒绝替换 dataset identity 后的 pending probe。额外的
`tests/rolling_ops.py` 只测试纯 admission 拒绝；其中合成 ELF 字节不执行，
不计作真实兼容性证据。

## 复现与回执

在 Linux/WSL 中准备已核验的 Rust toolchain、Docker Linux engine 与
已缓存的固定 PostgreSQL 镜像。旧、新编译器须一致；本轮使用 Rust
**1.95.0**，MSRV 1.86 验收是另一项证据，不能混淆。

```bash
python3 examples/operations/archive_binary.py target/rolling-old-01 \
  --repository "$PWD" --toolchain 1.95.0

# 必须是实际用于新 Cargo build 的冻结源目录及已审核 binary SHA。
python3 examples/operations/binary_witness.py target/rolling-new-01.json \
  --binary /ABS/PATH/taskloom --expected-sha APPROVED_BINARY_SHA \
  --source /ABS/PATH/FROZEN_BUILD_SOURCE --toolchain 1.95.0

sha256sum target/rolling-old-01/report.json target/rolling-new-01.json
python3 examples/database/postgres_fixture.py create target/rolling-pg-01
python3 examples/operations/rolling_rehearsal.py target/rolling-run-01 \
  --old-report target/rolling-old-01/report.json --old-report-sha OLD_REPORT_SHA \
  --new-report target/rolling-new-01.json --new-report-sha NEW_REPORT_SHA \
  --postgres-fixture target/rolling-pg-01 \
  --fault-test /ABS/PATH/operational_faults-TEST_HASH --fault-test-sha FAULT_ELF_SHA
python3 examples/database/postgres_fixture.py remove target/rolling-pg-01
```

上述 hash 占位符必须替换为实际批准值；fixture/输出目录每次用新名称。
`fault-test` 来自当前 `cargo test --all-features --test operational_faults
--no-run` 的实际 ELF，不是 CLI。更换源文件或重编新 ELF 后应重新生成
相应见证并重跑，不能把旧候选的成功记录说成新源码已经验证。

总回执为 `report.json`；每后端还保留 gate、逐 phase 五表原始快照与 SHA、
worker PID/ELF、signal/退出码、queue events、冷读记录和 fixture 请求分母。
源/二进制/日志、私有数据库 URL/凭证所在目录均留在忽略的 `target/` 或
宿主缓存中，不将整份运行目录作为开源制品。密码只经子进程环境传递。

WSL 若有独立 native Docker daemon，须明确选用已核验的 engine/客户端
连接并记录来源，不改默认 context/daemon。本轮临时通过 Docker Desktop
Unix proxy 访问已有固定镜像，仍只创建/删除确切 ID/nonce label/image
匹配的 owned 容器及复制库。

本地通过范围是同 schema、同实际执行契约的 worker 替换/回滚与拒绝
不兼容输入；不代表多机网络分区、任意新旧 feature 共存、API/UI 控制面
滚动、live schema upgrade、零停机或长期 SLO 已完成。

## 最终源码的独立重跑

2026-11-28 在 durable MCP 与文档多样性增量冻结后，另建 immutable
source/build witness 并重跑；没有复用早期候选结果。最终 CLI ELF SHA 为
`8e996d20152f57f6c134f7ffc463555c2bdb12b1fb00c2d728231786f532dcf2`，
独立当前 SDK fault ELF SHA 为
`b0641a98922f8c673d10b3185358305d558154f602d4139519fb14b3af244226`。
全部 Rust build inputs 在构建前、构建后、冻结复制时一致；Linux 编译器
仍为 Rust 1.95.0，不将其他平台的 MSRV 检查归入此轮。

最终轮的 SQLite、PostgreSQL 各 13/13 后端检查与 3/3 全局检查通过；
各 13 个健康任务对应 13 次实际夹具 HTTP 请求，无失败或重复。未知预算/
效果、实际 old/new 交叠、回滚冷读、不兼容 schema/dataset/config 拒绝均
重新验证。共同启用的执行 profile 仍为上文限定的无工具文本角色；新包
包含 MCP/diversity，不意味着旧 worker 能执行新协议或任意 feature。

原始报告保留于 ignored `target/same-schema-rolling-final-20261128-1/`，
随后对同一最终 ELF 完成[真实断网与容量演练](network-capacity-drills.md)。
自建 PostgreSQL 容器与匿名数据卷已按确切所有权回执删除，未操作其他容器。
