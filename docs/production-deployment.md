# SQL 服务部署基础

这个部署包提供可构建镜像、Compose 和 Helm 模板，以及正式的多用户服务 CLI。
它使用实际 SQL 身份、项目配额与事务队列，不用 offline 模型冒充生产推理。
配置和健康检查不调用模型；只有已授权任务被 worker 接收后才可能产生模型费用。
模板不代表云端部署、跨机器故障转移、吞吐或生产 SLO 已经验收。

## 服务入口和健康检查

启用 `app,database` 后可运行：

```bash
taskloom service-check /etc/taskloom/host.json
taskloom service-serve /etc/taskloom/host.json
taskloom health-check http://127.0.0.1:8080/health/ready
```

`service-check` 读取宿主注册的配置，检查身份库、SQL schema/dataset 和显式启用的
队列；不加载模型、MCP、embedding 或遥测凭据，不执行迁移。`service-serve` 加载
原生 provider 配置、核对队列的宿主绑定后才监听。默认挂载多用户 JSON API；
显式 `console:true` 可开放 `/console` 中文项目工作台。单操作员 API 和身份管理
API 不挂载；Bearer、成员关系与角色每次仍由 SQL 检查。

`/health/live` 只返回进程存活布尔值；`/health/ready` 在三秒边界内检查实际身份库、
项目 schema/dataset 和队列宿主绑定。SQL 不可用、schema 不匹配、队列被禁用或
服务正在退出时，ready 返回 503。健康响应不披露 SQL URL、配置、用户或密钥。
ready 不是模型/工具网络健康，也不证明空闲 worker 正在处理任务。

用户私有记忆 CRUD 和私有生成是两个独立的宿主开关。启用 `private_memory`
不会自动允许模型调用；需要明确配置 `private_execution`，例如
`{"max_active_runs":2,"max_retained_jobs":256}`，并启用项目生成配额、配置每运行
预算及模型价格。当前私有会话为 `no-tools-v1`：不继承共享工具、技能或写权限，
不支持私有写工具的审批/重放流程。`/api/private/...` 所有者取自已认证用户，
私有历史不进入共享项目列表、队列或诊断。

私有控制器的并发限额独立于共享队列 `max_running/max_open_jobs`，两者不能相加
当成同一项目并发策略；它们的模型金额和在途/未知尝试都由同一个 SQL 项目生成
配额事务约束。每个逻辑模型步骤重新检查成员关系和撤销；已经接收的模型步骤
仍遵守原有 Provider 内部重试/降级契约，不能把这描述为每次 HTTP 尝试的原子撤销。

服务和 worker 同时处理 Ctrl+C 与 Unix SIGTERM，停止新派发并走既有持久化取消/
退出路径。强制 SIGKILL、超出容器 grace 或网络故障仍可能产生未知模型/外部效果；
必须保留日志、等待 lease 失效并按既有核对流程恢复，不重置预算或自动重放写入。

## 本地 Compose

从仓库根目录构建：

```bash
docker build --file deploy/Dockerfile --tag taskloom:local .
```

默认镜像编译 `app,database`。需要 OIDC 浏览器登录、签名技能或 Office loader 时，
构建时显式选择对应 feature；运行时仍须配置各自独立宿主开关：

```bash
docker build --file deploy/Dockerfile --tag taskloom:local --build-arg TASKLOOM_FEATURES=app,database,oidc,skill-registry,office .
```

`browser_login` 的固定端点、公钥、SQL user 映射和私有状态边界见
[登录说明](login-service.md)。API 与 ID-token audience 分开配置。编译开关不等于
启用网络登录，也不自动安装或激活技能。

Dockerfile 使用官方 Rust **1.86.0**、`--locked` 多阶段构建。运行镜像以 UID/GID
10001 运行，只包含原生程序、可信的单次管理程序和 CA；没有模型权重、密钥、
Docker socket、宿主 shell 授权或隐式 sandbox。PDF/自定义工具等额外可执行文件
需要另行审阅镜像；不能把 Docker socket 挂入 API 或普通 worker。

先审阅 `deploy/config/project.json` 中的真实 provider、模型、计价、每运行预算和
工具权限。示例使用 OpenAI-compatible API，价格只是配置示例，部署者必须核对实际
模型/账户价格；不能把示例价格或美元单位套到不兼容的订阅。当前示例无任何工具授权。
`queue.json` 为四个同时运行、二十四个未关闭任务；`quota.json` 为 USD 1 的累计
项目生成预算与四个在途/不确定尝试，二者不含 embedding、外部工具和基础设施费。

将模型密钥配置在本机环境变量（例如 `MY_MODEL_KEY`）后，可在仓库外创建私有文件：

```bash
python examples/operations/prepare_compose_secrets.py /absolute/private/taskloom-secrets --model-key-env MY_MODEL_KEY
export TASKLOOM_SECRETS_DIR=/absolute/private/taskloom-secrets
docker compose --file deploy/compose.yaml config --quiet
docker compose --file deploy/compose.yaml up --detach database
docker compose --file deploy/compose.yaml run --rm migrate
docker compose --file deploy/compose.yaml run --rm admin taskloom-admin quota-set team main /etc/taskloom/quota.json 0 deployment-operator "reviewed initial generation ceiling"
docker compose --file deploy/compose.yaml run --rm configure-queue
docker compose --file deploy/compose.yaml run --rm admin taskloom-admin put-user /etc/taskloom/operator.json 0 deployment-operator
docker compose --file deploy/compose.yaml run --rm admin taskloom-admin issue operator /var/lib/taskloom/operator.private-token deployment-operator
docker compose --file deploy/compose.yaml up --detach --scale worker=2 api worker
```

Windows PowerShell 用 `$env:TASKLOOM_SECRETS_DIR='C:/absolute/private/taskloom-secrets'`
代替 `export`。脚本不发请求或打印密钥，目录与文件为 create-only；POSIX 文件模式
0600，Windows 必须审阅私有目录 ACL。管理程序发行的 Bearer 只写入指定新私有文件，
不输出到日志。安全地交付给相应操作员并定期撤销；不要放在 YAML、URL、模型输入或 Git 中。
原生 Linux 的 Compose file secrets 可能保留宿主 UID/mode，不能假设 long syntax
会替 bind-mounted 文件改属主：保持私有父目录权限，并由部署管理员明确给予 UID
10001 对 `database-url`/`model-key` 文件的读取权（例如审阅后改属主，维持 0600）。
不要通过全局可读权限来绕过容器读取失败。

`migrate`/`configure-queue`/`admin` 是显式管理 profile，不在普通 `up` 时自动执行。
现有环境必须先用 `catalog`、`quota-get`、`taskloom queue` 检查当前 CAS revision，
示例的 revision 0 只适用于首次初始化。失败时检查原因，不循环重置或覆盖现有数据。
Compose 的两个 worker 不超过 SQL `max_running=4`，SQL 才是跨进程最终 admission。
端口仅映射到宿主 loopback；远程使用必须配置可信 TLS 反向代理和访问控制。

## Helm 和 Kubernetes

Chart 位于 `deploy/helm/taskloom`，不创建数据库、云资源或 Secret。提前审阅镜像并用
不可变 digest、外部 PostgreSQL TLS URL 和真实模型凭据创建 `existingSecret`：

```bash
kubectl create secret generic taskloom-secrets --from-file=database-url=/private/database-url --from-file=model-key=/private/model-key
helm lint deploy/helm/taskloom
helm template taskloom deploy/helm/taskloom --values /private/reviewed-values.yaml
```

生产 values 必须设置自建镜像 registry/digest，并审阅所有 provider/价格/预算。
Secret 仅通过明确 `secretKeyRef` 注入进程。ServiceAccount 不自动挂载 token，
没有 Role、ClusterRole 或 Kubernetes API 权限。容器 non-root、只读根目录、
drop ALL capabilities、RuntimeDefault seccomp；API 与 worker 是两个 Deployment。
SQL profile 的必需状态全部在数据库，本地 emptyDir 仅作有界工作目录，不是备份。

Service 是 ClusterIP HTTP；外部 TLS 反向代理或 ingress 由宿主显式提供。
默认 NetworkPolicy 只允许带约定 namespace 标签的入口与 DNS；必须审阅
`networkPolicy.allowedEgressCIDRs` 为 SQL、模型及明确工具放行所需网络。
标准 NetworkPolicy 不能按主机名过滤，不能声称这等同于域名权限或 source 工具授权。
没有审阅 egress 就不能接通数据库/模型；不要为了“能跑”开放不受限网络。

默认 API 以 HTTP ready/live 探针分别处理依赖与进程；worker 的 exec readiness 是
无派发的冷 SQL 检查，不装假的 HTTP 健康端点，也不在 SQL 短暂故障时反复重启
健康 worker。监控实际队列进展、失败/等待状态和 lease，而不是把 probe 当任务成功率。

### 显式静止迁移

API/worker Deployment 使用 `Recreate`，schema 迁移绝不放在 pre-upgrade hook。
它不自动停止其他 release/主机，操作员必须先停止**全部**使用该数据库的旧 API/worker，
关闭 admission、备份数据库/宿主资产并确认没有 live lease。

```bash
helm upgrade --install taskloom deploy/helm/taskloom --values /private/reviewed-values.yaml --set maintenance.enabled=true --set api.replicas=0 --set worker.replicas=0
# 等待旧 Pod 全部退出，并检查任何其他部署/主机的 lease；完成备份。
helm upgrade taskloom deploy/helm/taskloom --values /private/reviewed-values.yaml --set maintenance.enabled=true --set api.replicas=0 --set worker.replicas=0 --set migration.enabled=true --set migration.jobId=reviewed-v3-001
kubectl wait --for=condition=complete job/taskloom-taskloom-migrate-reviewed-v3-001 --timeout=300s
```

Job 不自动重试。失败后保留日志，核对原数据库状态，不删除不确定用量；不得擅自
重跑。首次初始化队列可额外设置 `migration.configureQueue=true` 和正确的
`queue.expectedPolicyRevision`，但项目 generation quota/身份管理仍是明确的
可信管理操作，使用相同镜像内 `taskloom-admin`，不暴露管理 HTTP 端点。
完成冷检查、配额/队列和身份初始化后，将 `migration.enabled=false`、
`maintenance.enabled=false` 并恢复已审阅 replica 数。回退应用版本不等于 schema 降级。

### PDB 和 HPA 的准确范围

PDB 默认 `maxUnavailable=1`，可约束自愿驱逐；只有一个 API Pod 时它仍允许全部
API 暂不可用，不应称为高可用。它不防节点故障，不阻止显式 scale-to-zero 维护。
HPA 默认关闭。可选 `autoscaling/v2` 使用真实 CPU utilization，要求集群有
Metrics Server 且容器配置 CPU request；不是一个尚未实现的队列 backlog metric。
模型请求经常等待外部 I/O，低 CPU 不代表无积压，开启前必须用自己的流量校准。

worker 固定 replica 和 HPA 最大数必须不超过 `queue.maxRunning`；Chart 会拒绝
超过的配置。修改 Chart 值不会改写数据库已冻结的队列策略；先做明确 CAS 更新。
SQL cap仍是最终并发限制，多开的 worker 不会让模型费用或外部效果自动安全。
开启 HPA 时不要依赖手动 `replicas`，维护前先关闭 API/worker HPA。

## 本地验收和诚实限制

```bash
python examples/operations/accept_host.py target/NEW-host-check --taskloom target/debug/taskloom
python examples/operations/validate_deploy.py target/NEW-template-check --helm /path/to/helm
python examples/operations/accept_compose.py target/NEW-compose-check --image taskloom:REVIEWED-LOCAL-TAG
```

该脚本使用真实 SQLite、实际 Taskloom 子进程与一个只计数并拒绝请求的 loopback
provider。要求零模型请求、无模型凭据的冷检查、queue 未配置拒绝、SQL schema
破坏后 ready=503/live=200、恢复、未认证项目 API=401、单操作员 root=404，
POSIX 还验证实际 SIGTERM 成功退出。Windows 明确只终止本脚本的 idle 子进程，
不冒充 Linux SIGTERM 验收；没有模型效果、真实 cloud 或生产 SLO 的假结果。

当前实测与日志在发布验收记录中列出。Helm 渲染/静态策略检查不代表已经部署 Kubernetes；
Dockerfile 因网络或基础镜像不可用而未构建时必须报告，不能改用离线 demo 称为生产镜像。
原有数据库备份、迁移、负载和原生 worker 生命周期证据仍见 `durable-queue.md`，
与本次基础设施 smoke 分开记录。

`validate_deploy.py` 需要 Helm、Compose v2 和 PyYAML；实际渲染并解析 ConfigMap JSON、
Secret 引用、non-root/capabilities、无 Kubernetes API grants 和迁移/HPA 的拒绝边界。
`accept_compose.py` 仅对唯一 nonce 项目启动真实 PostgreSQL、API 和两个 idle worker，
使用从未提交任务的临时无效模型 key，不把它当成原生推理验收；随后核对实际容器
限制和 SIGTERM 退出，清理该项目的容器/卷与三份准确路径的临时 secret 文件。
它不停止用户现有容器，不安装云服务，不提交模型任务。
