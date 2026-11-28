# 浏览器 OIDC 授权码 / PKCE 登录

`app,oidc` 提供显式配置的浏览器登录闭环。宿主固定 authorization/token
端点、注册 callback、client ID 和 ID-token 公钥策略；用户点击登录后由
浏览器前往 IdP。框架不会在后台自动登录用户或发现新的信任端点。

实现使用 UUID v4 的操作系统随机源、SHA-256 的 PKCE S256 challenge，
JWT 密码学仍由 `jsonwebtoken` 验证器负责。没有自定义 JWT 签名验证。

## 配置和宿主集成

`BrowserLoginConfig` 必须明确提供：

| 字段 | 含义 |
| --- | --- |
| `authorization_endpoint` / `token_endpoint` | 固定可信 HTTPS 端点，不能由请求改写 |
| `client_id` | 已注册客户端 ID |
| `client_secret_env` | 可选宿主环境变量名；使用 `client_secret_post`，值不进入配置预览/响应 |
| `redirect_uri` | 已注册完整 HTTPS callback，路径以 `/auth/callback` 结尾 |
| `success_path` | 登录后的固定同站相对路径，例如 `/console`，不接受请求提供 return URL |
| `pending_ttl_seconds` | 1–600 秒待登录有效期 |
| `session_ttl_seconds` | 1–86400 秒本地会话最大有效期，同时受 ID-token 到期限制 |
| `max_pending` / `max_sessions` | 明确有界的待处理/活跃会话容量 |
| `allow_http_loopback_for_tests` | 默认 false；仅本机 fake IdP/开发夹具显式开启 |

生产端点拒绝 URL 凭据、query、fragment 和非 HTTPS。fixture 模式只允许
HTTP loopback；不会允许普通远程 HTTP。生产 cookie 使用 `__Host-` 前缀、
`Secure; HttpOnly; SameSite=Lax; Path=/`，无 Domain。HTTP fixture 使用明确
的 `taskloom-dev-*` cookie 名和非 Secure cookie，不能当作真实 TLS 验收。

宿主构造专用 ID-token `OidcVerifier`，其 audience 必须等于 `client_id`。
该 verifier 的 audience 通常与 API JWT audience 不同，不要求共用 API
bearer verifier。issuer/subject 仍只能通过固定映射绑定已存在的 SQL user。

```rust,ignore
let login = BrowserLogin::new(configuration, id_token_verifier, identities)?;
let project_api = project_api.layer(
    axum::middleware::from_fn_with_state(login.clone(), login::cookie_bridge)
);
let router = project_api.merge(login::router(login.clone()));
```

只在项目 API 上挂 cookie bridge。独立 bearer-only API 继续可用；它不要求
所有客户端改用 cookie。ServiceHost 可使用 `browser_login` 的
`{configuration,id_token_policy}` 配置，console 的交互仍由宿主开启。

## 请求过程和防护

1. `GET /auth/login` 创建随机 state、nonce、浏览器绑定和 PKCE verifier，
   将 S256 challenge 发送给固定 authorize URL。服务器只返回跳转和绑定
   cookie，不调用 IdP 登录接口、不接受客户端 endpoint/redirect/grant。
2. `GET /auth/callback` 必须同时匹配 state 和 HttpOnly 浏览器绑定。
   状态由互斥锁一次消耗，进入短期 tombstone 后才进行**一次** token POST。
   重复/并发 callback、超时、失败都不能重新交换原授权码。
3. token 请求包含固定 client/redirect、实际 code、保存的 verifier，可选
   client secret。禁止 HTTP redirect 和自动重试，响应上限 1 MiB、15 秒超时。
4. 签名验证后再次要求 nonce 匹配；issuer、audience、时间、jti 和显式用户
   映射沿用 OIDC bearer 契约。多 audience ID-token 需要正确 `azp`；若有
   `azp` 也须等于 client ID。`at+jwt` 不可冒充浏览器 ID-token。
5. 对已映射 SQL user 签发短期本地 `tlm_` 凭据，只保存在服务器内存。
   浏览器仅获得不透明 session cookie，JWT/access/refresh/local bearer 均
   不返回 JavaScript、不写入 localStorage、不放进 Location 或日志。

当前 ID-token policy 沿用显式 `jti` 要求；不带 jti 的标准 OIDC IdP 尚未
提供兼容适配。支持的是已配置、满足这些 claim/算法约束的 IdP 子集，不能
称作与所有 OIDC provider 兼容。当前仅支持 client_secret_post 或公共客户端，
不伪装支持 Basic/private_key_jwt、动态注册或 token refresh。

已有有效浏览器会话时，另一次登录/回调拒绝覆盖它；失败 callback 不清空
其他活跃 cookie。多个标签页的待登录绑定不会因另一个 callback 成功被
清空，但 state、内存和 tombstone 始终受容量和 TTL 限制。

## Cookie API、CSRF 和注销

- `GET /auth/session` 返回 `{authenticated,user,csrf,expires_in_seconds}`。
  csrf 是专用于请求绑定的值，不是认证 bearer。响应 `Cache-Control:no-store`。
- cookie 模式的 POST/PUT/PATCH/DELETE 必须携带**精确同源** `Origin` 和
  `x-taskloom-csrf`。跨源/缺少/重复 Origin 或不匹配 CSRF 都拒绝。
- cookie 与 Authorization 同时出现时拒绝冲突，不能挑一个较宽权限身份。
  header-only 原路径不变。console 切换 cookie 登录时应清除旧内存 bearer。
- bridge 每次先检查服务器 session 和 SQL 本地 token，再交给既有项目
  middleware 检查当前 user enabled、membership 和角色。JWT roles/groups
  不授权，移除/降低 SQL 角色不会被旧 token claim 覆盖。
- `POST /auth/logout` 要求同样的 Origin/CSRF，清除服务器会话并 SQL revoke
  对应 local token，返回过期 cookie。GET 不执行注销。
- 宿主关闭时调用 `BrowserLogin::shutdown`：拒绝新登录/bridge、清 pending，
  撤销现有 local sessions；正在交换的旧 state 不重试，也不在关闭后发布
  新 session。服务器重启丢失内存会话，不能凭旧 cookie恢复 token。

本地会话失效不会“回滚”已经派发的工作。IdP 远程注销/撤销和 refresh token
生命周期不由本地实现自动获知；即时限制可用 SQL disabled user/移除角色、
local token revoke 或关闭对应登录宿主。JWT jti/key 配置变更不是对已经
换发的 local token 的远程撤销承诺。

所有 auth 响应使用 no-store、no-referrer 和限制性 CSP。代理/访问日志仍须
单独对 callback 的授权码 query 脱敏；框架没有审计或改变外部代理日志。
SQLite 身份 catalog 保留 token/audit 历史，过期不会自动抹除记录；长期高频
登录需要显式归档/容量运营，不能把有界 catalog 当成无限认证服务。

## 本地验收与未验证项

`tests/app_login.rs` 启动真实 loopback HTTP fake IdP、Taskloom cookie/API
router 和真实 SQLite。fixture 验证实际 authorize 参数、code challenge 与
verifier、签发 nonce JWT、callback、session cookie、CSRF、项目角色和注销。
还覆盖错误浏览器绑定、并发/重复 callback 只交换一次、错误 nonce、过期
JWT、错误 azp、access-token 类型混淆、IdP 503 不重试、pending 容量/过期、
重复输入和 shutdown。签名 key 仅为公开测试夹具，绝不是真实 IdP 凭据。

```powershell
cargo test --locked --all-features --test app_login --test app_oidc --test native_providers
```

这些是真实本地协议/存储验收，不是云 IdP/TLS/跨浏览器/SLO 验收。正式
投产仍需目标 IdP 的授权、client secret 管理、HTTPS reverse proxy、浏览器
回跳、cookie 域策略、登录容量、SQL catalog 和运营撤销测试。进程间共享
登录 session、持久 refresh、发现/自动公钥轮换尚未提供。
