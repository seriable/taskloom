# App / SQL 签名技能注册表强制执行

显式启用 `app,skill-registry` 后，App 可以把本地签名注册表作为安装与
激活门槛，同时继续使用既有 File 或 SQLite/PostgreSQL `AppSkillStore`。
包内容仍是惰性数据；安装、签名或信任 publisher 都不授予工具、凭据或审批。

```json
{
  "skill_registry": "E:/protected/taskloom-registry"
}
```

这是完整 AppConfig 的片段。该路径是宿主选择的信任/回滚 ledger，不能
来自模型、HTTP 请求或包内容；加载配置时相对路径按配置目录解析。默认
`None` 不序列化，保留旧 unsigned local 行为；没有 `skill-registry` feature
却配置它会拒绝启动。保护 ledger 的文件权限、备份和恢复，删除/回滚它
也会丢失反回滚记忆，不能把它当临时 cache。

## 显式管理与激活

```powershell
taskloom --config app.json skill-registry
taskloom --config app.json skill-registry-trust reviewed-trust.json 0
taskloom --config app.json skill-registry-install ABS_SOURCE_DIR release.json package.tar 1 0
```

最后两个数字分别是实际 registry revision 和 skill catalog revision。
trust/release 签名格式、作者工具和 key 管理见 [签名注册表](skill-registry.md)。
CLI 使用当前宿主时间检查 expiry，metadata/archive 只能是单个安全文件名，
路径遍历、符号链接/Windows reparse、超限和不匹配内容在派发/安装前拒绝。
这些命令只打开配置的存储，不初始化模型/MCP/embedding/telemetry 凭据。

启用此策略后，普通 `skill-install` 被拒绝，避免把 unsigned install 的
成功提示误认为已获 registry 认可。SDK 管理员仍可直接访问原始 SQL/File
store；原始存储接口不是请求授权接口。即使包由 SDK 直接写入，App 新激活
也必须有完整 registry 安装 receipt，不能用 catalog 中存在对象替代签名。

`skills::load` 的每个根包和最多 31 个依赖都在同一 registry 锁范围内验证：
原始签名/包 pin、当前信任、撤销、expiry、版本/sequence 水位以及完整安装
receipt；再交给既有 store 的依赖解析、catalog CAS 和宿主 grants 检查。
raw SQL 安装的未签名依赖会让根包激活失败。签名不能扩大 `SkillHost` 的
工具效果、permission、credential names，不能改审批或消费 ceiling。

## 两套存储不是一个事务

注册表 ledger 是有 OS 锁和 CAS revision 的本地文件；技能 catalog/object
可以是 SQL 事务。实现复用同一个 `RegistrySkillStore` 抽象和原注册表逻辑，
不复制另一条未经验证的安装路径。顺序固定为：

1. 核对 archive、manifest、signature、trust、expiry、monotonic watermark。
2. 持久保存水位及 `installed:false` pending receipt。
3. 使用调用方明确的 catalog revision 执行 File/SQL install CAS。
4. 只有 store 成功后才持久标为 `installed:true`。

SQL 冲突/异常、进程中断、提交成功却丢失确认，或最后 receipt 写入失败，
都不会倒退水位、删除已写 SQL 对象、换成旧版本或自动重试。pending 对象
仍是惰性数据，不能供新激活使用。操作员应检查实际两边 revision，再以
**完全相同的 release** 显式重试；相同 digest 的已提交安装可幂等完成，
再补齐 receipt。不得编辑旧签名/sequence 或把未知提交当作未发生。

这不是跨文件/SQL 原子提交，也没有为多个机器的独立本地 ledger 建立
一致性。单机多个进程共享同一保护路径有 OS 锁；多机需显式共享可信状态
及对应运维/备份设计，当前实现不宣称分布式 publisher trust 服务。

## 撤销和恢复

新 Active 激活必须满足当前 key/digest 信任和最新水位。已安装旧 pin 的
`RecoveryOnly` 仍要求历史 completed receipt 和实际 package/dependencies
存在，但不会因为后续 key/digest 撤销或过期而删除原对象。其模式只允许
原运行恢复，不允许新模板工作流；原宿主 grants、原冻结绑定、精确审批和
未知副作用规则照常检查。撤销不是删除旧运行、篡改账本或放宽恢复权限。

已经加载的进程持有原技能快照，不会因 ledger 文件更新被后台改写或立即
终止。每次新 App/激活重新检查。多挂载整体原子更新、自动热撤销和远程
安装/信任 API 尚未提供，不从本地锁推导生产服务承诺。

## 本地证据

`tests/skill_registry.rs` 新增真实 SQLite + App/CLI 控制：根包已签名但 raw
SQL 依赖无 receipt 时拒绝；依赖签名安装后 App 可加载；撤 key 后 Active
拒绝、RecoveryOnly 原 pin 保留；启用策略后 CLI 拒 unsigned install。
另一个夹具先真实 SQL commit 再模拟丢失确认，检查 pending receipt、水位、
禁止激活和 exact-release 幂等恢复。它不是伪造 SQL 成功或跨存储原子性。

```powershell
cargo test --locked --all-features --test skill_registry --test app_skills
```

这些检查没有模型、网络 registry 或云服务请求。SQL 抽象支持 PostgreSQL，
本增量真实执行的是 SQLite；不能把代码复用当作本轮实际 PostgreSQL 验收。
当前没有生产 publisher、公共注册表、自动下载、签名质量背书或信任扩权。
