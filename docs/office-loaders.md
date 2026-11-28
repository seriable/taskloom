# XLSX / PPTX 有界只读加载器

启用可选 `office` feature 后，SDK 提供 `XlsxLoader`、`PptxLoader` 和
`OfficeLimits`，使用已固定版本的 ZIP / XML 库读取本地 OOXML 文件。默认最小库
不增加这些依赖。既有 `DocxLoader` 保留原有提取策略和兼容范围，本增量不改变它。
这里没有启动 Office、LibreOffice、脚本、宏、公式引擎或网络下载器。

```rust,no_run
# #[cfg(feature = "office")]
# async fn example() -> Result<(), taskloom::runtime::RuntimeError> {
use taskloom::runtime::rag::{DocumentLoader, XlsxLoader, PptxLoader};
let sheet = XlsxLoader::default()
    .load(std::path::Path::new("policy.xlsx"), "policy-sheet", "政策表格").await?;
let deck = PptxLoader::default()
    .load(std::path::Path::new("policy.pptx"), "policy-deck", "政策演示").await?;
# let _ = (sheet, deck);
# Ok(()) }
```

## 文件范围与拒绝策略

宿主明确选择文件，加载器读取 regular file 的有界快照；不解压到文件系统。
路径和文件权限由宿主管理，符号链接的本地源路径会按现有加载器行为 canonicalize。
ZIP 内部链接、设备、管道、加密项、未知压缩方式均被拒绝。仅接受 Stored / Deflate。
绝对路径、盘符、反斜杠、空路径段、`.` / `..`、控制字符和 `%/?/#` 歧义路径、
大小写折叠重复路径均拒绝。已知 VBA、ActiveX、嵌入对象、externalLinks、macroSheet
及 `.bin` part 被拒绝；这是一份保守的受支持格式子集，会拒绝部分正常 Office 文件。

所有 entry（包括未使用的媒体和元数据）均有界读取至 EOF，检查 CRC、实际与声明大小。
所有 `.xml` / `.rels` 均检查 UTF-8、XML 1.0、配对结束标签、唯一根节点、
展开命名空间后的重复属性及结构上限；DTD、实体声明、未知实体、非法 XML 字符、
处理指令和不支持的编码拒绝。内置与数字 XML 字符引用会解码。命名空间按 URI
解析，支持不同前缀；不能靠同名 local tag 绕过受信 namespace。支持的是 transitional
OOXML URI，strict OOXML namespace、非 ASCII XML element/attribute name 不在此子集内。

默认上限为：

| 项目 | 默认 | 允许配置的硬上限 |
| --- | --- | --- |
| 压缩文件 | 8 MiB | 8 MiB |
| ZIP entry 数 | 256 | 1024 |
| 单 entry 解压大小 | 8 MiB | 8 MiB |
| 全部解压大小 | 32 MiB | 64 MiB |
| XML 深度 | 128 | 128 |
| 全包 XML element 总数 | 100,000 | 200,000 |
| sheet / slide 数 | 128 | 256 |
| cell、shared string、paragraph / table cell 合计提取项 | 50,000 | 100,000 |
| 最终 UTF-8 文本快照 | 8 MiB | 固定 8 MiB |

每个 element 最多 256 个属性、属性原始值最多 64 KiB、namespace URI 最多
1024 字节、XML name 最多 256 字节。namespace declaration 按层保存，URI 共享，
避免为每个子节点重复复制祖先映射。解析在现有 blocking worker 内运行；这些
是输入和内存结构上限，不是独立进程、CPU deadline 或恶意文件隔离沙箱。

## 关系、顺序与外部内容

要求 `[Content_Types].xml` 与 `_rels/.rels`；根 officeDocument relationship
必须精确指向支持的 `xl/workbook.xml` 或 `ppt/presentation.xml` 及对应非宏内容类型。
每个使用的 worksheet、sharedStrings、slide part 也必须有正确的显式内容类型。
所有 relationship parts（包括未使用的）检查唯一 ID、合法 source 和内部 target，
内部目标必须存在。相对 `..` 只允许在归档内部规范化，不允许逃出归档；本子集
不接受根绝对 target、URL 编码、query / fragment 或 URI scheme 形式的内部 target。

`TargetMode="External"` 只保留为不可访问的标记，完全不跟随 URL、本地文件或
UNC 位置；若 workbook / sheet / strings / slide 等结构依赖这个目标，直接拒绝。
普通未使用的外部 hyperlink 不妨碍提取可见文本。URL 文本始终是数据，不是授权。

XLSX 以 workbook `sheets` 顺序提取，PPTX 以 presentation `sldIdLst` 顺序提取，
不会按 ZIP entry 名称或文件编号猜测显示顺序。重复结构 ID、重复目标、缺失关系
或错误内容类型均拒绝。归档中未被这些结构引用的 part 不会自动加入输出。

## XLSX 文本快照

每个 sheet 输出包含序号、JSON 编码名称、可见状态和实际 part 路径的生成标签。
隐藏 sheet 也提取并标记状态。按显式 `row r` 和 `cell r` 处理 sparse grid；行号
必须递增且在 Excel 范围内，单行列号必须递增、唯一且不超过 XFD，不按最大行列
号分配稠密矩阵。地址与所在行不一致、重复和未给显式地址的 cell 拒绝。

支持 sharedStrings（含 rich text runs）、inlineStr、原始数字、布尔、字符串、
ISO 日期字符串与错误字符串。每个值以 JSON string 编码，保留 Unicode 与转义后的
换行 / 制表符，并标明 `row / cell / type`，例如：

```text
[sheet 1 name="政策" state="visible" part="xl/worksheets/sheet7.xml"]
row=2 cell=A2 type=s value="审批阈值"
row=2 cell=C2 type=n value="5000"
```

数字保持原始 XML 文本；不按样式应用货币、百分比或日期格式，不把 Excel serial
date 自动解释为日期。布尔保留 `0/1`，错误值是错误数据，不当作成功计算。
提取 cell 中出现 `<f>` 就拒绝整个加载，包含 cached value 也一样；不执行、更新
或把公式缓存当成已验证事实。合并单元格、格式、图表、数据透视表、外部 workbook、
计算链、打印区域及精确视觉布局不在本子集内，不能从 sparse cell 快照推导。

## PPTX 文本快照

每个 slide 输出顺序、原始 ID、part 和 `show` 标记，按 `spTree` 的 XML 顺序
遍历普通文本 shape 和 group。提取 DrawingML 段落、rich runs、显式换行；表格
按实际 XML row/cell 序号标记，多个段落在该 cell 的 JSON 值内用换行连接。
示例：

```text
[slide 1 id=257 part="ppt/slides/slide2.xml" show="1"]
shape=1 paragraph=1 text="审批说明"
shape=2 table_row=1 table_cell=1 text="金额"
```

动态 `<a:fld>` 拒绝，不信任日期、页码等缓存字段。不运行动画、链接或嵌入对象。
不提取 notes、master/layout 继承文本、SmartArt、图表标签或图片；没有 OCR、字体、
空间阅读顺序、文本遮挡、表格跨格展开或演示渲染保证。生成 shape 序号只是本快照
中被支持的文本/graphicFrame 遍历序号，不等于 PowerPoint 的 shape object ID。

## Provenance 与导入

`Document.source` 包含格式 / 提取版本、原 ZIP SHA-256、提取文本 SHA-256；
metadata 还保存配置原文及其 SHA-256。媒体或未提取 part 改动也会改变 archive
绑定与 chunk 身份，即使输出文本相同。标准 chunk offsets 和引用验证指向完整
提取快照中的 UTF-8 byte range，不是压缩文件、XML、原 Office 页面的字节范围。
定位标签和 JSON 编码是生成结构；不能把标签当作原作者文字。引用审核需同时
查看提取策略及原文件，不将可复制的原文片段等同于观点正确或文件渲染结果。

`app,office` 提供显式本地 CLI：

```powershell
taskloom --config app.json ingest-office C:/reviewed/policy.xlsx policy-sheet "政策表格" xlsx
taskloom --config app.json ingest-office C:/reviewed/slides.pptx policy-deck "政策演示" pptx
```

最后一个参数只能为 `docx / xlsx / pptx`，不按扩展名自动猜测。CLI 使用加载器默认
上限；SDK 可明确降低或调整 `OfficeLimits`。未编译 `office` 时，在读取配置与
源文件前拒绝该命令。命令只打开配置存储，不初始化模型、embedding、MCP、工具或
telemetry 凭据；不会执行后续付费索引构建。它先读取 corpus revision，再解析文件，
最后以该 revision 执行原有 ingest CAS；解析失败不写 corpus，并发修改会冲突，
不能静默覆盖。File 和现有 SQL corpus 抽象都复用同一路径，本轮真实 CLI 验证为 File。

这是有本地文件权限的可信宿主管理命令，不是向普通 HTTP 请求开放服务器任意文件
读取。当前 HTTP/UI 没有 Office 上传、远程路径导入或每用户文件授权流程；AppConfig
和宿主工具 grants 没有因此改变。

## 本地验证与剩余差距

```powershell
cargo test --locked --all-features --test agent_office --test agent_docx
cargo clippy --locked --all-features --test agent_office --test agent_docx -- -D warnings
```

测试在临时目录创建真实、可解析的小型 Office ZIP（含关系/类型/worksheet/slide），
检查顺序、稀疏坐标、Unicode、转义、文本/表格与精确快照引用；独立修改公式、
地址、关系、namespace、DTD、重复路径、宏、unused part CRC 和各容量上限，检查
失败时既有 corpus 不变。普通外部 hyperlink 控制同时监听本机端口确认未连接。
cold CLI 使用不存在的模型密钥配置完成实际导入，拒绝公式后不改变 corpus。
既有 DOCX 回归单独运行。测试不是 Microsoft Office / LibreOffice 兼容性认证，
没有启动 Office、外部模型或真实 Office 文件的跨版本渲染验收。

仍缺严格 OOXML / 更宽格式兼容、XLSX 样式与日期解释、图片/OCR、复杂 PPTX 内容、
浏览器上传及容量/权限 UX、独立进程解析隔离和真实文档语料验收；本包不据此将
Office、OCR 或总体框架对齐标为完成。
