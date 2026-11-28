# Taskloom TypeScript / JavaScript 客户端

Node.js 20+ / Fetch 客户端，提供类型声明、超时/取消、精确审批、私有会话和
有界响应。没有自动重试、重定向、凭据发现或隐式恢复。无法精确表示的服务端
整数会明确拒绝，避免错误审批 revision。浏览器调用仍受服务端认证与 CSRF 约束。

当前为源码预览包，`private:true` 防止意外发布。完整用法和限制见
[SDK 文档](https://github.com/seriable/taskloom/blob/main/docs/client-sdks.md)。

English: explicit Fetch client with TypeScript declarations, bounded responses,
timeouts and cancellation. No automatic retries or redirects. Unsafe numeric
revisions are rejected rather than rounded. Inspect unknown outcomes before
resubmitting operations.

MIT OR Apache-2.0；随包包含两份许可证。
