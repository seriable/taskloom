# 原生 Provider 协议

新增 `NativeProvider`（`openai` feature）和应用配置 `provider.type = native`。
这些是单次、非流式、无隐式重试的协议适配器；GatewayTransport 继续负责
价格、预算持久预留、失败分类、并发和不确定用量。真实云账号兼容尚未验收。

```json
{
  "provider": {
    "type": "native",
    "protocol": "responses",
    "base_url": "https://api.openai.com/v1",
    "key_env": "OPENAI_API_KEY",
    "namespace": "reviewed-account-project"
  },
  "model": "HOST_SELECTED_MODEL",
  "output_tokens": 2048
}
```

这是配置片段，还须提供与实际账号一致的价格和显式生成预算。CLI/API
配置审阅不读取密钥、不联系服务；UI提供原生协议选择。工具仍来自宿主
注册表，服务返回的函数名/参数不能授予权限。

| protocol | base_url | 认证/边界 |
| --- | --- | --- |
| `responses` | 到 `/v1` | Bearer；`store:false`、不使用 previous_response_id、只接受宿主函数工具，不启用服务端 built-in tools。 |
| `gemini` | 到 `/v1beta` | `x-goog-api-key`；模型名为单个 identifier，工具调用保留 correlation。 |
| `vertex` | 到 `/v1/projects/PROJECT/locations/LOCATION/publishers/google` | 宿主提供 OAuth Bearer；不自动获取/刷新 ADC、访问 metadata endpoint 或执行 gcloud。 |
| `bedrock` | `https://bedrock-runtime.REGION.amazonaws.com` | 宿主显式提供 Bedrock Bearer API token；不是 SigV4/STS 或 AWS credential-chain 实现。 |
| `azure` | 完整 `/openai/deployments/DEPLOYMENT/chat/completions?api-version=VERSION` | `api-key`；`model`须与显式 deployment一致，按这个冻结身份配置价格；不自动发现部署或改 api-version。 |

HTTPS强制，HTTP仅loopback允许。没有跟随重定向、URL内凭证、自动fallback
端点或原始响应错误体回显。请求/响应有独立字节上限；reported usage在
无效工具参数、拒答和截断时仍进入网关核算，缺失usage保持未知。

目前范围是文本生成和原生函数工具往返。图片、seed、流式、原生embedding
和某些provider专属结构明确拒绝，不会默默降级成另一协议。Responses
的不可表示reasoning/encrypted输出、Gemini/Vertex的thoughtSignature
目前拒绝；这避免在旧Message模型中丢掉必须保留的状态，不能称这些模型的
全部推理模式兼容。Bedrock结构化JSON/schema在typed任务路径暂拒绝，
可使用Text协议和宿主校验；不假装所有模型都有同一JSON能力。

Azure、Gemini、Vertex、Bedrock实际账号的权限/配额/区域、模型版本、
token刷新和参数差异，必须在部署者环境另行验收；本地loopback协议测试
不能替代真实账号。参考 `tests/native_providers.rs` 的实际HTTP请求、
拒答/截断/未知block/usage/鉴权/重定向回归。
