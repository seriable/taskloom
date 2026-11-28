# 保守的受管单活灾备 v1

`examples/operations/managed_dr.py` 提供一个明确受限、人工触发的 PostgreSQL
灾备控制面。它使用**两个区域数据库及第三个独立 PostgreSQL witness**，将源 SQL
访问与受管进程真正隔离后，做全量静止检查点、恢复、验证和新 epoch 晋升。
它不是自动故障切换服务，也不能在源不可达、无法证明 fencing 时继续晋升。

当前执行适配器只接受 `examples/database/postgres_fixture.py` 创建、具有精确
container ID / nonce label / image ID 的本地 Docker PostgreSQL，以及明确 image ID、
deployment label 的 Taskloom worker 容器。三台 PostgreSQL 必须拥有不同物理 system
identifier；witness 不随区域数据库备份、克隆或恢复。这里的 a/b 是逻辑区域，同一
Docker 主机的实测不能替代真实地域、网络分区、Kubernetes、DNS 或流量切换验收。

## 状态与实际隔离

首次 `initialize` 只在独立 witness 创建一个权威状态及追加审计，不能覆盖原表。
初始 app role 必须是独立的 LOGIN、NOINHERIT、非 superuser/createdb/createrole/
replication/bypassrls、无任何角色成员关系；应用只获得所需 DML 权限，schema 由
管理员管理。witness 写入强制 `synchronous_commit=on`，三个 PostgreSQL 必须启用
`fsync` 和 `full_page_writes`。

一次 `promote` 的顺序如下：

1. 对完整旧 witness 状态做 equality CAS，递增 epoch，并把 `Prepare/ready=false`
   与审计放进同一个 SQL transaction。旧状态、并发控制器或已有 Unknown 均拒绝。
2. 对源 app role 执行 **NOLOGIN**，终止其**全部实际 PostgreSQL session**，确认
   role 已禁用且 session 为零。拒绝同一数据库残留的其他客户端连接。
3. 按本 deployment 的精确 container ID、image ID、labels 停止全部受管源 worker，
   检查实际运行容器携带的数据库 URL/epoch 与待隔离 app role 完全一致，再复核 SQL
   隔离。URL 的实际 SQL username/region/database 必须匹配，不能仅凭 JSON 中的
   role 字段放行混入管理员 URL。容器停止可在超时后强制结束；不把强杀表述为 graceful drain。
4. 等待全部实际 `taskloom_leases` 按**源数据库时钟**自然到期，最长 330 秒；不改
   owner、epoch、预算、job、unknown 或外部工具状态来制造可恢复结果。
5. 封存完整资产根及原生 full `pg_dump -Fc --no-owner --no-privileges`。封存前后
   比较所有文件及数据库 schema/全部实际表行/sequence 业务状态/数据库 locale。
6. 在目标创建**新的 NOLOGIN role 和全新 database**，恢复全量 dump 与全部资产，
   验证 schema/行/sequence/locale 与文件摘要。目标新角色不会提前获得 LOGIN。
7. witness CAS 到新 epoch 的 `Active/ready=false` 后才启用目标 LOGIN。使用新的
   私有 epoch 凭据执行实际 Taskloom `queue` cold read；随后在精确拥有的临时容器
   启动仅容器 loopback 可访问的 `serve`（不启动 SQL queue worker），以一次认证
   `GET /api/queue` 比较真实 Application 的 host_binding 与恢复的 queue policy。
   确认实际 frozen binding 一致并清理 probe 后才 CAS `ready=true`。
8. 独立 `start-worker` 只有在精确匹配当前 witness `Active/ready=true` 时才能启动。
   新进程使用相同 `/managed/config/project.json`、`../state` 和配置中的数据库
   环境变量名，恢复的 dataset UUID、模型/工具/role/queue 冻结绑定不会被重写。

任一副作用、SQL 提交或返回确认失败，控制器会尝试将**同一 operation** 标为
`Unknown/ready=false` 并保留审计/备份；若 witness 也不可达，私有本地拒绝记录会保留
“提交确认未知”。不自动补偿、不重试晋升、不重新开启旧 role、不退款或重放工具。
Unknown 必须由管理员对照三个数据库、实际容器和外部账单/效果进行人工核对；v1
不提供一个跳过核对的 `resume`/`force-promote` 命令。不要直接修改 witness JSON 来
“清错”。最终提交确认丢失不证明目标从未执行过；并发操作者/已启动进程与外部效果
同样需要核对，Unknown 不是远端撤销协议。

**回滚也是反向的新晋升**：A→B→A 使用 epoch 1→2→3、全新目标 database 和全新
role。历史 role 永远保持 NOLOGIN；不能让旧进程缓存的密码再次访问旧数据克隆。
源隔离只能阻止新的 SQL durable admission，不能撤回已发往远端的调用。已持久化的
未知预算、效果与待人工恢复状态必须原样保留。

## 全资产与秘密边界

host 私有 `spec.json` 必须显式列出九个独立分类：`configuration`、`state`、
`signed_registry`、`qdrant_journal`、`rerank_ledger`、`source_refs`、`secret_refs`、
`secrets`、`external_effects`。每项给出 managed root 下的 `paths`，或写明
`absent_reason`。每个实际文件必须恰好属于一个分类；漏项、重叠、缺失声明路径、
`..`、symlink、Windows junction/reparse point、特殊文件一律拒绝。

这覆盖整个真实文件根，而不是只导出五张 Taskloom SQL 表。数据库 fingerprint 也会
枚举全部实际 public 表；当前保守 profile 拒绝非 public 关系、views/partitions、
用户函数/独立自定义类型/额外 schema、用户collation/operator/文本检索配置、额外
extension/language、large objects、用户trigger/rule/RLS、FDW、role/database自定义GUC、event triggers 和
replication publication/subscription 等未验目录。文件上限为 4096 个、每文件 64 MiB、合计 256 MiB；数据库
物理大小及逻辑行快照各不超过 256 MiB。超出上限应显式扩展并重新验收，不能静默截断。

运行时秘密采用三个私有文件：

- `secrets/deployment.json`：本 epoch 的 `role`、`database`、`url`；恢复时仅此文件
  派生为新数据库、新 role 和新密码，原件仍保留在 sealed backup。
- `secrets/runtime-env.json`：host **显式选定**的实际运行时秘密映射；不会将宿主全部
  环境复制进 worker，拒绝 PATH/loader/proxy/Docker 等环境注入。
- `secret_refs/runtime.json`：`{"version":1,"names":[...sorted names...]}`，精确包含
  上述映射及 `TASKLOOM_DR_DATABASE_URL`；配置中的 `*_env` 与 stdio environment
  引用必须全部出现在这里，漏掉模型 key/token 等引用即拒绝。

临时 binding probe 另使用一次性随机 bearer，只经子进程环境传递，不复用用户凭据。
它使用受审 Debian worker image 已有的 `/bin/bash` 固定 `/dev/tcp` HTTP probe；无
host shell、curl 下载或安装。生产适配其他 image 时必须显式重新验证该入口。

以上文件、所有原始 dump、manifest、数据库、WAL、日志和输出目录都是**私有运维
数据**。工具不会输出密码/SQL/响应正文；公开 receipt 只有摘要、数量、epoch/region
和检查状态。不要提交 `target/`，也不要将私有 manifest/dump 当作开源发行附件。
POSIX 使用文件权限和目录 fsync；Windows 的 chmod 不是 ACL 管理器，必须先确保
输出根 ACL 私有。所有备份文件执行 fsync，但当前本地 drill 不证明断电/宿主磁盘损坏
时的恢复能力。实际持久卷、异地复制、加密、保留期、密钥托管和恢复介质可靠性仍需
部署方设计。

配置、文件路径、程序/image、数据库管理员和 Docker daemon 都是受信任的 host
输入。工具不能自动发现放在根外的 ledger、未声明服务、外部进程、远程 Qdrant/S3
内容或外部效果账本；启用前必须把实际文件状态归入完整根，并保留独立服务的精确
引用与相应恢复策略。此工具复制 journal/ref，不把它当成远程 collection/object 的
完整备份。实际云模型、IdP、KMS、object/vector 服务可用性也不由 cold queue read
认证。v1使用明确owned internal Docker网络，不会自动开放公网模型/外部工具路由；
生产网络与数据库server/HBA配置必须另行设计和复验。Linux bind mount 的 UID/ACL 应保证镜像 UID 10001 只获得所需访问；本轮真实
环境为 Windows Docker Desktop，不能据此声称所有 Linux 权限布局均已验。

## 本地重复验收

先构建包含 `app,database` 的受审镜像和 `operational_faults` 集成测试可执行文件。
drill 必须传递本地镜像 immutable ID，禁止隐式 pull；它会创建并最终删除本轮精确
拥有的三台 PostgreSQL、内部网络及 worker，不操作其他容器。原始私有证据保留在
独立输出目录。

```powershell
python tests/managed_dr.py
python examples/operations/managed_dr_drill.py target/managed-dr-local `
  --worker-image sha256:<explicit-local-image-id> `
  --fault-test target/debug/deps/operational_faults-<hash>.exe
```

演练先运行真实硬杀 worker 的 PostgreSQL operational fault 测试，产生 actual unknown
generation/effect 状态与已 fsync 的外部效果；然后由真实 Taskloom worker 完成源任务，
执行 A→B 的完整晋升和目标任务，再 B→新A。它比较全数据库与完整资产，验证历史密码
连接失败、源 worker 实际停止、旧 CAS/旧 worker startup 拒绝、源暂停不可达拒绝、
已提交 Active 后显式注入 lost ACK 导致 Unknown，并拒绝继续受管 startup。

可选 registry/Qdrant/rerank/source-ref 分类的本地 sentinel 仅测不透明字节的完整封存，
不冒充这些外部系统真实业务验收。实际 SQL/queue/unknown/effect 来自真实 Taskloom
二进制。最终应以绑定最终源码、最终镜像 ID 和脚本 SHA 的新 receipt 为准，不能将
旧候选 image 结果扩大成当前全功能版本或生产多区域保证。
