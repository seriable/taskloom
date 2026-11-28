# 显式 OIDC JWT bearer 验证（第一包）

本文保留第一包 API bearer 验证的实现和验收边界。后续已加入独立、显式配置的
[浏览器 authorization-code / PKCE 登录](login-service.md)，包括 callback、
nonce、session cookie 与 CSRF；下文“本包未提供”指第一包本身。真实外部
IdP、TLS、多节点会话及自动 key rotation 仍不能从本地夹具推导完成。

`oidc` 是可选 feature，默认最小库不启用。验证使用
`jsonwebtoken = 9.3.1` 的签名实现，关闭 PEM/simple_asn1 默认依赖，不手写
密码学或从模型/请求字段信任用户身份。与 `app` 一起使用时提供
`runtime::app::oidc` 模块。

这一包是**固定信任配置的 JWT API bearer 验证**，不是完整浏览器 SSO：
没有 authorization-code/PKCE 登录页、callback、nonce/session cookie、
discovery、远程 JWKS 自动下载、refresh token、IdP introspection、自动用户
注册或从外部 groups/roles 赋权。未运行真实云 IdP，不能宣称完整 SSO 已验收。

## 信任边界

宿主建立 `OidcPolicy` 并构造 `OidcVerifier::new(policy)`：

- `issuer` 为固定 HTTPS URL，不允许 query、fragment 或嵌入凭据。
- `audience` 明确指向 API 的预期受众。
- `algorithm` 固定为 `RS256` 或 `EdDSA`，不会按 token 任意切换算法。
- `jwks` 是宿主提供的 public JWKSet 快照。仅接受最多 32 个唯一 `kid`、
  正确 `alg`、`use:"sig"`、公钥字段和可选 `key_ops:["verify"]`。拒绝
  HMAC/oct、私钥字段、远程证书/URL 和混合算法配置。
- `bindings` 显式映射 `{issuer,subject,user}` 到**已存在**的本地 SQL user。
  issuer/subject 必须精确匹配，不能用邮箱、显示名或外部 role 自动创建用户。
- `revoked_jti` 是宿主显式撤销列表，最多 4096 个 ID。
- `max_token_lifetime_seconds` 为 1–86400 秒，限制签发到到期的最大跨度。

配置文件可序列化，但 `OidcVerifier` 和 `VerifiedOidcSubject` 没有可反序列化
的认证捷径。后者没有公开构造函数，只有成功的签名/claim 校验能产生它。
原始 JWT 不保存在认证 actor 中，Debug 输出不显示 bearer 或 subject。

```rust,ignore
use taskloom::runtime::app::oidc::OidcVerifier;

let verifier = OidcVerifier::new(reviewed_host_policy)?;
let actor = verifier.authenticate(&sql_identities, bearer_from_authorization).await?;
let role = sql_identities.authorize(&actor, &registered_project, permission).await?;
```

HTTP 宿主必须对每个请求重新调用验证和 SQL 授权，不能缓存一次验证后的
actor 作为永久会话。JWT 不能选择项目 Application、数据库、工具授权或
记忆 owner；这些仍由已认证 SQL 用户及注册项目决定。

## 每次验证的要求

bearer 上限 16 KiB。Header 只接受 `alg/kid/typ`：拒绝未知 critical 扩展、
`jku`、`jwk`、`x5u`、`x5c` 等，`kid` 只能从固定 key map 选择，绝不触发
网络请求。支持 `JWT` 或 `at+jwt` 类型；不接受 `none` 或 HMAC 混淆。

签名之外要求 `iss/sub/aud/exp/iat/jti`，验证可选 `nbf`，采用零时钟宽限：
过期、未来 `iat/nbf`、过长有效期、未映射 subject、被撤销 jti 都拒绝。
JWT 中的 roles、groups、tenant、user 等额外 claims 不产生本地权限。

成功映射后，SQL 再检查本地 user 存在且 enabled；既有 `authorize` 每次
重新读取当前 membership 和数据库时间，并再次检查外部 token 的到期时间。
本地禁用用户、移除项目、降低角色会影响下一次授权。角色更新后不能复用
JWT 里的旧 role 声明覆盖它。

## 轮换与撤销

公钥和撤销列表是冻结的宿主配置，轮换需要构造并显式替换 verifier；不从
未知 `kid` 自动刷新。先加载经过审阅的新旧公钥快照、随后撤下旧 key 的
部署策略由宿主决定。自动 watcher、跨节点原子更新和远程 IdP key rotation
时效尚未提供验收。

本地 `tlm_` token 的 revoke API 管理本地 token；**不会声称撤销外部 JWT**。
外部 JWT 可通过显式 jti denylist/撤下公钥、或立即 SQL disabled user/移除
membership 来限制后续访问。IdP 远程注销或撤销不会被本地离线验证器自动
获知。JWT 已过验证后才执行的操作也不会被撤销“回滚”。

直接 SDK `authenticate_oidc_subject` 是宿主集成点，要求不可伪造的已验证
subject proof，再选择显式本地 user mapping；它不是允许 HTTP 请求自己
提交 user ID 的注册/登录接口。HTTP 应使用 `OidcVerifier::authenticate`。

## 本地验证与未完成项

`tests/app_oidc.rs` 使用专门生成、公开在测试文件里的 Ed25519 测试 key 和
JWT fixture，没有真实凭据、浏览器登录或外部 IdP 调用。测试覆盖：签名
篡改、issuer/audience/subject、缺字段、exp/iat/nbf、算法混淆、未知 kid、
header URL、恶意/私钥 JWKS、显式公钥轮换、jti 撤销；真实 SQLite 覆盖
外部管理员 claim 不扩大本地 Reader 权限、跨项目拒绝、实时角色更新和禁用。

```powershell
cargo test --locked --all-features --test app_oidc --test app_multiuser --test app_private_memory
```

本包不接受为完整企业 SSO、IdP 互操作、RS256 全厂商兼容、滚动 key 更新、
PKCE callback 或生产审计认证。后续需要真实 IdP 与浏览器闭环，以及多节点
配置轮换/撤销验收；这些不能从本地签名 fixture 推导通过。
