# Python 和 TypeScript 客户端

源码客户端在 `sdks/python` 和 `sdks/typescript`；不代表已经发布 PyPI/npm。
Python 需要 3.10+，TypeScript/JavaScript 使用 Node20+ 内置 fetch。没有自动
重试、模型选择、权限或恢复策略；每个请求只派发一次。

```bash
python -m pip install ./sdks/python
```

```python
import os
from taskloom_client import Client

client = Client("http://127.0.0.1:8080/api", os.environ["TASKLOOM_API_TOKEN"])
client.run("example", "Summarize the available sources")
state = client.inspect("example")
```

```javascript
import { TaskloomClient } from './sdks/typescript/index.js';
const client = new TaskloomClient('http://127.0.0.1:8080/api', process.env.TASKLOOM_API_TOKEN);
const result = await client.inspect('example');
```

多用户服务显式使用 `/tenants/T/projects/P/api` 作为 base，bearer 必须属于
被授权用户，宿主服务决定 actor。固定 local bearer 不是多用户身份认证。
精确审批用 `approve(id,ticket,actor,note)`，不会自动恢复；审核输入后再调用
`resume(id)`。事件分页返回服务端 `next`，用它作为 `events` 的 after。
列表接口当前受服务端1000记录上限，SDK不伪造无限分页。
`semantic_recall(text)` / `semanticRecall(text)` 是独立的显式付费入口，要求
宿主开启语义记忆并具备 Submit 权限；它会向宿主 embedding 服务发送本人
已审核的记忆值和问题，普通 `recall` 与 session turn 不自动调用它。
JavaScript的Number无法精确保留所有u64；客户端拒绝不安全的大整数而不
舍入revision/预算。需要这类值时使用Python/Rust或额外的无损JSON适配器。

客户端拒绝 HTTPS 以外的远程明文 URL、URL 内凭证及重定向；响应有字节
限制，错误消息不回显服务端 body。传输中断、超时或本地 abort **不证明**
服务端未执行，返回 outcome_unknown 类型错误；先检查持久运行和效果，
不得自动重试提交/审批/外部写入。JS支持每请求 AbortSignal，Python同步
客户端支持socket timeout，取消服务器任务使用显式 cancel 操作。
Python 客户端禁用环境代理，校验声明的响应长度并拒绝截断内容、NaN/Infinity
及浮点溢出；调用者无需把凭据交给环境中未经此客户端配置的代理。

本地契约验证：

```bash
python tests/sdk_client.py
node --test sdks/typescript/test.mjs
```

这些测试使用真实loopback HTTP，检查输入/路由/审批、身份header、错误、
响应上限、取消和重定向，不产生模型或外部服务请求。Rust服务的端到端
集成与云端/真实用户验收需要另外记录。

源码包准备已实际执行 Python wheel 构建与 npm pack dry-run，两个包都包含
MIT/Apache-2.0 许可证和说明。TypeScript 保持 `private:true` 防止意外发布；
没有发布到 PyPI/npm。发布版本、账号和兼容声明仍需独立审阅。
