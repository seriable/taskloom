# 源码发行准备、checksum 与离线签名

`examples/release/source_release.py` 只准备 **Git 源码归档**、真实 source manifest、
SHA-256 checksum 和 Ed25519 离线签名/验证。它不创建维护者身份或私钥，不提交、
打 tag、推 GitHub、发布 registry、构建或运行被归档源码，也不签本轮尚未冻结的工作树。

签名证明“显式公钥验证了这份源码身份及精确归档字节”，不证明软件质量、测试通过、
模型效果、安全审计、许可证合法性或该公钥属于哪位维护者。技能注册表签名与这里的
源码发行签名有不同 domain / schema，不能互换。

## 环境与输入

需要 Git、Python **3.11+**（标准库 `tomllib`）和已明确安装、支持 Ed25519 的
`cryptography`；本地控制使用 cryptography 49.0.0。工具不会自动安装依赖或调用网络。
这与客户端 SDK 的 Python 3.10+ 要求不同。

所有命令要求明确、可信的本地 Git worktree 根目录。完整读取 commit/tree/index 与
原始 blobs，核对实际 tracked 文件、模式和未忽略的 untracked 文件；不相信用户填写
的“clean=true”或 source hash。提交 HEAD、暂存区和原始工作树必须完全一致。

这里是比普通 `git status` 更严格的 **raw source profile**：不执行自定义 clean/LFS
filter，不隐式把 CRLF、编码或其他转换变成 Git blob。Windows 的 autocrlf 工作树若
原始字节不同会拒绝；请由宿主另选 `core.autocrlf=false` 的干净 checkout 来准备发行，
不要为了通过检查改写正在开发的文件。LFS smudged 内容、稀疏缺失文件、symlink、gitlink
与子模块不属于本子集。源码不会从 `.gitattributes` 做 export-subst/export-ignore。

Git 命令直接读取本地 object/index，无 shell、source script、hook、filter 或 fsmonitor；
清除环境中的 Git override，关闭 Git lazy-fetch / replace objects，禁止 transport 协议，
不借缺失 object 自动 fetch 或运行 credential helper。缺失对象直接失败。
工作树检查前后核对 observed drift；它不是跨进程/分布式原子工作树冻结服务，准备发行
期间仍需宿主停止编辑并保护 Git / executable / 输出目录。

## 准备 source bundle

```powershell
python examples/release/source_release.py prepare --repo C:/reviewed/taskloom --out C:/reviewed/taskloom/target/source-release-NEW
```

输出目录必须不存在，父目录已存在；位于 worktree 内时必须明确被 Git 忽略。
同样限制适用于签名输出。失败的部分新输出保留供检查，不覆盖/自动重试旧结果。

输出文件：

| 文件 | 精确内容与边界 |
| --- | --- |
| `source.tar.gz` | 按 Git 路径排序的 regular files；统一 `source/` prefix、Git file mode、commit timestamp、uid/gid 0、空 uname/gname；PAX tar 与无 filename/当前时间的 gzip；**压缩最多16 MiB** |
| `source-manifest.json` | canonical UTF-8 JSON；真实 HEAD commit/tree/clean、实际 Cargo package/version、MSRV 1.86、default features []；每个文件的路径/模式/大小/raw SHA-256；精确 artifact SHA-256/大小/格式；明确不作质量/身份认证 |
| `SHA256SUMS` | 从实际压缩 artifact 全字节计算的 SHA-256 与固定文件名，不接受自报 checksum |

最多 4096 个 regular source files、单文件最多16 MiB、总 source bytes 最多64 MiB、
未压缩 canonical tar 最多80 MiB、manifest 最多4 MiB。所有读取均有上限；Git 子进程
有输出上限和单次30秒时限。解析归档时只进行有界完整 gzip 解压，与本地真实 Git blobs
重建的 canonical tar **逐字节比较**，不解压到文件系统，不信任归档提供的路径/权限。

受支持路径必须是有界 UTF-8 相对路径，无 traversal、盘符、反斜杠、控制字符或大小写
折叠重复。拒绝明显的编译包/二进制、runtime database、credential/key 路径，以及
target/node_modules/cache 等目录。PNG 等经过审阅的源码测试资产可包含在 source tree。
这份有限 profile 不是通用 secret scanner；宿主仍需在 commit 前审阅全部公开源码，
不能把 Git tracked 就等同于可以公开。

Cargo 声明必须实际为 `rust-version="1.86"`、`features.default=[]`。工具不把这两个
元数据声明当作实际编译测试；实际兼容证据和未验证项另见
[兼容矩阵](source-compatibility-matrix.md)，最终测试 receipt 由发行方另外审核。

## 使用既有维护者私钥签名

```powershell
python examples/release/source_release.py sign --repo C:/reviewed/taskloom --bundle C:/reviewed/taskloom/target/source-release-NEW --private-key C:/private/maintainer-ed25519.raw --out C:/reviewed/taskloom/target/source-release-NEW/source-signature.json
```

私钥只能是宿主显式给定的 **32-byte raw Ed25519 private seed**，不是 PEM、密码、
环境自动发现或从 source/archive 读取的 trust。工具只做 bounded、regular/non-symlink
读取，不生成/写入/打印/提交私钥。私钥必须位于 bundle 外，且不能是该 source tree
中 tracked 的文件。宿主负责 key 来源、私有目录 ACL、备份、撤销和发行身份验证。

签名先重新验证真实 Git 状态、manifest、完整 artifact 与 checksum，再生成
create-only `source-signature.json`。packet 包含固定 format/algorithm/domain、精确
manifest/artifact SHA-256、公钥 SHA-256 与64字节签名的 base64；不携带可被自动信任
的公钥或 publisher 声明。

待签字节为：ASCII `taskloom-source-release-ed25519-v1`、单个 NUL、完整 canonical
manifest bytes（含末尾 LF）。manifest 内包含实际完整压缩 artifact 的 SHA-256，
所以任何 artifact 字节改变都会使验证失败。签名 domain 与 skill registry 不同。

## 显式外部公钥验证

```powershell
python examples/release/source_release.py verify --repo C:/reviewed/taskloom --bundle C:/reviewed/taskloom/target/source-release-NEW --signature C:/reviewed/taskloom/target/source-release-NEW/source-signature.json --public-key C:/trusted/maintainer-ed25519-public.raw
```

必须给出独立可信来源的 **32-byte raw Ed25519 public key**。工具不会从 archive、
manifest、签名包或网络发现 trust；公钥文件必须位于 bundle 外。调用者应事先用可信
渠道核对其 SHA-256；packet 的 key hash 是绑定检查，不会自动建立身份。

验证必须有对应 commit 的干净本地 checkout：它不只校验签名，还重新核对 source
commit/tree/index、Cargo 声明、完整 source archive 与 checksum。验证较旧 release
时应明确选择对应 checkout，不能拿后来 HEAD 的新文件冒充旧源码。没有网络依赖。

重复/额外 JSON 字段、非 canonical JSON、错误 key、跨 domain 签名、错误长度、
源码/元数据/checksum/压缩包/signature 的任何篡改、超限或 Git dirty 状态均拒绝。
验证失败不提取、不执行、不导入、不修复输入，也不自动重试。

## 本地方法测试与真实发行身份的区别

```powershell
python tests/source_release.py
```

测试只在临时 Git repository 中生成并销毁明确的 **ephemeral test keys**，测试
prepare/sign/verify、实际 Git dirtiness/index/HEAD 与 Cargo 声明、精确字节/源身份/
签名/domain/key 篡改、create-only、16MiB及gzip上限、危险source入口和不执行 Git filter。
没有为 Taskloom 当前工作树签名，也没有创建维护者身份或发布任何源码 / registry。

最终维护者签名、公开 source archive / checksum / sanitized 验收 receipt、真实 tag /
GitHub 地址 / registry 账号仍需发行方选择和实际执行。测试私钥、公钥或签名绝不能
被展示为项目维护者的信任凭证。
