# Taskloom Python 客户端

Python 3.10+ 的显式 JSON API 客户端，无第三方运行时依赖。支持有界响应、
超时、精确审批和私有会话；没有自动重试、重定向、凭据发现或隐式恢复。
令牌只发送到初始化时固定的服务地址。未知网络结果需要检查原 Job/Run，
不能自动重新提交非幂等操作。

当前为源码预览包，未发布到 PyPI。完整配置、示例和限制见
[SDK 文档](https://github.com/seriable/taskloom/blob/main/docs/client-sdks.md)。

English: explicit, bounded Taskloom JSON API client. No automatic retries,
redirects or credential discovery. Transport failures can leave the outcome
unknown; inspect the original operation before deciding how to recover.

MIT OR Apache-2.0；随包包含两份许可证。
