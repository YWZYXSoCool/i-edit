# i-edit 接入 rust-analyzer 方案

> 状态：草案 v1 ｜ 日期：2026-10-06 ｜ 基于当前工作区源码的静态分析，行号以当时版本为准

## 0. 定位与依赖的前置方案

本文档描述**如何把 rust-analyzer 的语义着色接进编辑器**。它不是独立的一件事，而是站在两个已有方案之上：

| 前置 | 出处 | 本文假定它已落地 |
| --- | --- | --- |
| 多色渲染能力（`Highlights`、`base`/`overlay` 两层、`replace_layer` 注入接口、`line_runs` 渲染接口） | [`color-rendering-plan.md`](./color-rendering-plan.md) | 是 |
| 主循环 poll 化 + `drain_incoming()` 异步入口 + 文档 `version` | 同上，Phase 2 / Phase 4 | 是 |
| 本地语法兜底层（syntect） | [`syntax-highlight-plan.md`](./syntax-highlight-plan.md) | 是（`base` 层） |

**分工**：rust-analyzer 只喂 `overlay` 层；`base` 层（syntect）负责 RA 未就绪时与非 Rust 文件的着色；`overlay` 盖住 `base`，没盖住的地方回退。

**本文档不实现任何 UI 新功能**（hover / completion / codeAction 都不在 v1），见 §9。

**已确定的前提（2026-10-06）**：base 层用 **syntect**；依赖不是约束（项目约束只有零分配），
故 `serde_json` 直接引入，暂不引 `lsp-types` / `url` / async 运行时（理由见 §10）。

---

## 目录

- [1. 目标与非目标](#1-目标与非目标)
- [2. 线程与进程模型](#2-线程与进程模型)
- [3. 连接与握手](#3-连接与握手)
- [4. 语义 token：解码与注入](#4-语义-token解码与注入)
- [5. legend → Style：映射与预计算](#5-legend--style映射与预计算)
- [6. 与编辑器的对接点](#6-与编辑器的对接点)
- [7. 诊断（L2）](#7-诊断l2)
- [8. 失败与降级](#8-失败与降级)
- [9. 分阶段与提交切分](#9-分阶段与提交切分)
- [10. 依赖决策](#10-依赖决策)
- [11. 风险与回归](#11-风险与回归)
- [12. 验收](#12-验收)
- [附录 A：消息流时序](#附录-a消息流时序)
- [附录 B：改动点清单](#附录-b改动点清单)

---

## 1. 目标与非目标

**目标**

| 级别 | 能力 | LSP 方法 |
| --- | --- | --- |
| L1（本文主体） | Rust 文件的**语义着色** | `initialize` / `initialized` / `textDocument/didOpen` / `didChange` / `semanticTokens/full` |
| L2 | 诊断波浪线 | `textDocument/publishDiagnostics` |
| L3 | 生命周期完善：切文件、关文件、重连、索引进度 | `didClose`、`$/progress`、`shutdown`/`exit` |

**非目标（v1 不做）**：hover、completion、codeAction、goto definition、inlay hints、rename。它们都需要新的 UI 形态（浮动窗口 / 虚拟文本 / 候选列表），属于独立方案；本文的传输层与协议层会为它们留出位置，但不提前实现。

---

## 2. 线程与进程模型

```
                 mpsc                     stdio
主线程（UI） ───────────────► 写线程 W ───────────► rust-analyzer 子进程
   ▲                                                     │
   │  try_recv（非阻塞，每帧一次）                        │ stdout
   │                                                     ▼
   └────────────── 读线程 R（阻塞读 stdout → 解析 → mpsc）
```

**必须两个额外线程，一个都不能省：**

| 角色 | 为什么 |
| --- | --- |
| 读线程 R | stdout 必须被**持续**读取。若没人读，子进程写满管道缓冲区后阻塞，进而不再处理我们的请求——经典死锁 |
| 写线程 W | 全量 `didChange` 的 payload 可能上百 KB，写大 payload 时若主线程阻塞在 `write` 上，UI 就卡了。写线程自己阻塞没关系 |

- 子进程用 `std::process::Command::new(cmd).stdin(piped()).stdout(piped()).stderr(null())` 启动；`Child` 由 **W 线程**持有（它负责 stdin，也负责退出时 kill）。
- 通道用 `std::sync::mpsc`（`Receiver::try_recv` 足够，不需要 async 运行时）。
- 请求 id：主线程自增 `u64`，`pending: HashMap<u64, PendingKind>` 记录请求类型，响应回来按 id 匹配。

**主线程侧的入口就是 `drain_incoming()`**（`app.rs`，color-rendering-plan Phase 4 已留好）：

```rust
fn drain_incoming(&mut self) {
    while let Ok(event) = self.lsp_rx.try_recv() {   // 非阻塞
        self.handle_lsp_event(event);                // 每个事件都置 needs_redraw = true
    }
    // 防抖：编辑静止 ~150ms 后发一次 didChange + semanticTokens/full
    if self.lsp_due() { self.request_semantic_tokens(); }
}
```

---

## 3. 连接与握手

### 3.1 启动

| 项 | 取值 |
| --- | --- |
| 可执行文件 | 环境变量 `I_EDIT_LSP` 指定，否则 `rust-analyzer`（PATH 查找）；`I_EDIT_LSP=off` 彻底关闭（参照 `main.rs:11-14` 的 `I_EDIT_LOG` 约定） |
| 触发时机 | 首次打开 `Language == Rust` 的**有路径**文件时惰性启动；scratch buffer 不启动 |
| 根目录 | `file_tree` 的 root → 文件父目录 → cwd |

### 3.2 消息帧（LSP base protocol）

```
Content-Length: <字节数>\r\n
\r\n
<JSON body>
```

- 读：循环读行直到空行 → 解析 `Content-Length`（大小写不敏感）→ 读 N 字节 → `serde_json::from_slice`。
- 写：`serde_json::to_vec` → 拼 header → `write_all` + `flush`（**必须 flush**，否则请求留在缓冲区）。
- `Content-Type` 可忽略，默认 UTF-8。

### 3.3 `initialize` 里必须声明的能力

不声明 = 服务端按最低能力给，可能拿不到 delta 或拿不到我们认得的 token 名。关键字段：

```jsonc
{
  "processId": <std::process::id()>,
  "rootUri": "<file:///...>",
  "capabilities": {
    "textDocument": {
      "semanticTokens": {
        "requests": { "full": { "delta": false } },   // L1 先用 full
        "tokenTypes":  [ /* 我们认识的名字清单，见 §5 */ ],
        "tokenModifiers": [ /* 同上 */ ],
        "formats": ["relative"],
        "overlappingTokenSupport": false,
        "multilineTokenSupport": false
      },
      "synchronization": { "dynamicRegistration": false, "didSave": false },
      "publishDiagnostics": { "relatedInformation": false, "versionSupport": true }
    }
  }
}
```

- `multilineTokenSupport: false`：我们不处理跨行 token，声明后服务端会自己切分。**这条很重要**，否则 `deltaLine` 之外的行内区间可能跨越换行，按行切 run 时会出错。
- `formats: ["relative"]`：简化解码（只有相对格式）。

### 3.4 握手序列

1. `initialize`（请求）→ 响应里拿到 `capabilities.semanticTokensProvider.legend` → **立刻预计算 `Vec<Style>` 索引表**（§5）
2. `initialized`（通知）
3. 当前文件 `textDocument/didOpen`（带 `languageId: "rust"`、`version`、`text`）
4. `textDocument/semanticTokens/full`（请求）→ 注入 `overlay` 层

---

## 4. 语义 token：解码与注入

### 4.1 delta 解码

响应 `data: Vec<u32>` 每 5 个一组：`[deltaLine, deltaStartChar, length, tokenType, tokenModifiers]`。

```rust
let mut line = 0usize;
let mut start16 = 0usize;
for c in data.chunks_exact(5) {
    line += c[0] as usize;
    start16 = if c[0] == 0 { start16 + c[1] as usize } else { c[1] as usize };
    let end16 = start16 + c[2] as usize;
    // 注：length 可能为 0（injected 等），start == end 的 run 直接丢弃
    ...
}
```

**注意 `deltaStartChar` 的语义**：`deltaLine != 0` 时是**相对行首**；`deltaLine == 0` 时是**相对上一个 token 的 start**。写错这一条，第一列之后的颜色会整体漂移。

### 4.2 UTF-16 → 字节

用 `highlight::utf16_to_byte`（color-rendering-plan §3.4），**只在注入时调用**，渲染路径不转换。

解码前先校验版本（§6.2）；版本不匹配直接丢弃整批，避免拿旧坐标去切新文本导致越界。

### 4.3 注入

按行分组（同一 `line` 的 token 天然递增）→ 构造 `Vec<Vec<StyledRun>>` → `Highlights::replace_layer(LayerId::Overlay, &rows)`。

- 构建缓冲由 LSP 模块自己持有（`Vec<Vec<StyledRun>>`，`clear` + 复用），稳态下第二次响应起零增长。
- 整批是"整份替换"语义：RA 的 full 响应覆盖全文，不需要做增量合并。

---

## 5. legend → Style：映射与预计算

### 5.1 为什么必须"按名字、不按下标"

服务端 legend 的实际内容随 rust-analyzer 版本变化（它会加自定义类型，如 `lifetime`、`formatSpecifier`、`injected`）。写死下标会在升级后静默错位。**按名字查表，未知名字回退 Plain**，才是最稳的。

### 5.2 预计算，别在循环里查哈希

`initialize` 响应到达时一次性建表：

```rust
// 按 legend 顺序，下标 = token type index，O(1) 查表，无哈希、无分配
type_styles:  Vec<Style>        // 长度 = legend.tokenTypes.len()
mod_styles:   Vec<Modifier>     // 长度 = legend.tokenModifiers.len()，只存"附加修饰"
```

之后每个 token 的样式 = `type_styles[t] + OR(mod_styles[每个置位的 modifier])`。

**规则（故意简化，避免组合爆炸）**：

- **颜色只来自 token type**；
- **modifier 只叠加 `Modifier`（粗体/斜体/下划线/暗淡），不改颜色**；
- 2^n 组合因此不需要预计算，`popcount` 次 OR 即可，零分配。

### 5.3 默认映射表（`src/lsp/theme.rs`）

按名字匹配，未列出的回退 `Plain`：

| token type | Style |
| --- | --- |
| `comment` | DarkGray + Italic |
| `keyword` | LightMagenta |
| `string` / `character` | LightGreen |
| `number` / `float` / `boolean` | LightYellow |
| `function` / `method` | LightBlue |
| `macro` / `attribute` / `derive` / `builtinAttribute` | LightRed |
| `struct` / `enum` / `union` / `typeAlias` / `interface` / `builtinType` | LightCyan |
| `type` / `typeParameter` / `constParameter` / `namespace` | LightCyan |
| `enumMember` / `variant` | LightYellow |
| `variable` / `property` / `field` | LightGray（默认前景） |
| `parameter` / `lifetime` | LightBlue 暗一档（Gray） |
| `selfKeyword` / `selfTypeKeyword` | LightMagenta |
| `operator` / `punctuation` / `*` 各类括号与 `angle` | Gray |
| `formatSpecifier` / `escapeSequence` | LightYellow（在字符串色上更亮，用 Bold 区分） |
| `unresolvedReference` | LightRed + Underlined（编译不过的符号，很有用） |
| `injected` | 不加修饰（交给内层语法） |
| `label` | LightRed |

| modifier | 附加 `Modifier` |
| --- | --- |
| `declaration` / `definition` | BOLD |
| `deprecated` | CROSSED_OUT（或 DIM） |
| `readonly` | 无 |
| `mutable` / `reference` | UNDERLINED |
| `async` | ITALIC |
| `documentation` | DIM |
| `injected` / `intraDocLink` | ITALIC |
| `unsafe` / `consuming` | UNDERLINED |
| `public` / `crateRoot` / `library` / `defaultLibrary` / `trait` / `static` / `constant` / `abstract` / `controlFlow` / `callable` / `attribute` / `modification` | 无 |

配色与 `syntax-highlight-plan.md` §7 保持一致（同一套 ANSI16 基调），避免 base / overlay 两层切换时颜色跳变。

**主题深化**留到后续：`I_EDIT_COLOR=truecolor` 时可换成真彩表（RA 的类型区分比本地扫描细，值得更精细的配色）。

---

## 6. 与编辑器的对接点

### 6.1 文件路径 → URI（Windows 上的坑）

`file:///C:/Users/...` 的正确编码是 `file:///c%3A/Users/...`：

- 盘符 `C:` → `/c%3A`（冒号必须 percent-encode，否则解析端会当成 scheme）；
- `\` → `/`；
- 路径段按 RFC 3986 percent-encode（空格 `%20`、中文按 UTF-8 逐字节 `%XX`）；
- UNC（`\\server\share`）→ `file://server/share`。

建议**手写约 40 行**（只做单向 `file://` 构造 + 解码），不引 `url` crate（见 §10）。必须补的单测：盘符大小写、空格路径、中文路径、末尾反斜杠、UNC、非 ASCII。

### 6.2 版本与防抖

| 项 | 取值 / 行为 |
| --- | --- |
| 文档版本 | 复用 `EditorState.version`（color-rendering-plan Phase 2），发给服务端时 `as i32` |
| 触发 | 编辑后**静止 150 ms** 发一次 `didChange` + `semanticTokens/full`；连续输入不刷请求 |
| 响应校验 | `result.resultId` 之外，用请求时记下的 version 比对；不匹配 → 丢弃整批（RA 很快，重算比纠错便宜） |
| 陈旧时的显示 | overlay 层保持上一次的数据直到新数据到达；缺失处回退 base（syntect），不会出现"花屏" |

### 6.3 同步方式：全量

`didChange` 发整篇文本（`SyncKind::Full` 语义：`textDocument/didChange` 的 `contentChanges` 给一条 `{ text: 全文 }`）。

- 理由：增量同步要自己算 range diff + 维护 UTF-16 位置，代码量与出错面都大；个人用途下全量的 payload（几十 KB 级）完全可以接受。
- 例外：超过 1 MB 的 Rust 文件直接不发（RA 会很慢，且 payload 大），回退 base 层。

### 6.4 文件切换

- 打开新文件：`didClose` 旧的（若它是 Rust）+ `didOpen` 新的；overlay 层 reset。
- 打开非 Rust 文件：不 `didOpen`，overlay 清空，只靠 base 层。
- scratch buffer（无路径）：不参与 LSP。

---

## 7. 诊断（L2）

`publishDiagnostics` → overlay 层的另一组 run：

- `severity` → 下划线颜色：`Error` 红、`Warning` 黄、`Information` 蓝、`Hint` 灰；
- `Style` = `Style::default().underline_color(color).add_modifier(UNDERLINED)`；
- 诊断区间可能**零长度**（行尾错误）：着色层已允许 `start == end`，渲染时画行尾一格（color-rendering-plan §10）；
- 多行区间：按行切分成多段 run；
- 与语义 token 同在 overlay 层 —— **冲突处理**：诊断优先（下划线是"更紧急"的信息）。实现上：注入时把诊断 run 放在语义 run 之后，渲染的 overlay 游标按"后者覆盖前者"解算即可（需在 `replace_layer` 里保证顺序，并在文档里写明这一约定）。

---

## 8. 失败与降级

| 情况 | 行为 |
| --- | --- |
| `rust-analyzer` 不在 PATH | 启动前用 `Command::new(cmd).spawn()` 试一次；失败 → 消息提示一次（`I_EDIT_LSP` 可指定路径），标记 LSP 不可用，**本次会话不再重试** |
| 启动后崩溃 / stdout EOF | 读线程发 `Exited` → 关闭 LSP、提示一次、overlay 清空、回退 base |
| `initialize` 超时（5 s） | 放弃，同上 |
| `semanticTokens` 超时（3 s） | 不阻塞：保留旧数据，允许下一次请求（防抖仍在跑） |
| 响应 JSON 解析失败 | 记 `warn!` + 丢弃该条，**不让编辑器崩**（LSP 是装饰层，出错只能降级不能致命） |
| 服务端能力不含 semanticTokens | 关闭着色链路，提示"服务端不支持语义 token" |
| 退出 | `shutdown` → 等响应（2 s）→ `exit` → `wait`（2 s）→ 超时 kill；线程 join 带超时，绝不挂住退出 |

统一原则：**LSP 是装饰层，任何故障都只能降级（回退 syntect base 层），不能影响编辑、保存与退出。**

---

## 9. 分阶段与提交切分

| 阶段 | 内容 | 预估行数 |
| --- | --- | --- |
| L1a | 传输层：`src/lsp/transport.rs` —— 帧读写、请求 id、pending 表、读写线程、子进程生命周期（可单测，不需真 RA） | ~200 |
| L1b | 协议层：`src/lsp/protocol.rs` —— initialize / initialized / didOpen / didChange / semanticTokens 的请求构造与响应解析（纯函数，可单测） | ~200 |
| L1c | 注入层：`src/lsp/theme.rs` + delta 解码 + `replace_layer` 调用；`uri.rs` | ~200 |
| L1d | 接线：`App` 持有 `LspClient`，`drain_incoming()` 处理事件、防抖请求、开关与降级 | ~120 |
| L2 | 诊断 → overlay run | ~80 |
| L3 | 文件切换、重连命令、索引进度显示（状态栏） | ~100 |

**提交切分**

1. `feat: lsp transport over stdio with reader and writer threads`
2. `feat: lsp handshake and semantic token requests`
3. `feat: legend to style table and token decoding`
4. `feat: feed semantic tokens into the overlay layer`
5. `feat: debounced requests, version checks and graceful degradation`
6. `feat: diagnostics as underlines`
7. `feat: file switching, reconnect command and indexing progress`

依赖（syntect、`serde_json`）在提交 1–3 期间引入，别和逻辑改动混在一个提交里。

---

## 10. 依赖决策

| 依赖 | 决定 | 理由 |
| --- | --- | --- |
| `serde_json` | **引入** | 手写 JSON 解析是本方案里唯一"明显不该自己写"的东西 |
| `serde` | 随 `serde_json` 引入（derive） | — |
| `lsp-types` | **暂不引** | 我们只用到 5 个方法；它会连带 `url` / `serde_repr` 等。等做 completion / codeAction / inlayHints 时再整体切换更划算 |
| `url` | **不引**，手写 `file://` 转换 | 只需单向构造 + 少量解码，约 40 行；且我们要的是"与 rust-analyzer 实际行为一致"的编码，自己写更好测 |
| async 运行时（tokio 等） | **不引** | 两个阻塞线程 + `mpsc` 足够；TUI 没有并发压力 |
| `syntect` | **引入**（base 层） | 见 `syntax-highlight-plan.md` §2 |

---

## 11. 风险与回归

| 风险 | 对策 |
| --- | --- |
| **写满 stdout 导致死锁** | 独立读线程持续读；这条不满足会挂死，是本方案第一风险 |
| Windows 路径 URI 不合法 → RA 不认文件 | 单测覆盖盘符 / 空格 / 中文 / UNC；握手失败时打 `warn!` 带上原始 URI |
| `deltaStartChar` 语义搞错 → 颜色整体漂移 | 单测：同一行多 token、跨行多 token、首列为 0、length 为 0 |
| UTF-16 与代理对（emoji 在字符串/注释里） | 复用 `utf16_to_byte` 的既有单测（ASCII / BMP / 代理对三档） |
| legend 版本差异 | 按名字查表 + 未知名回退 Plain + `warn!` 列出未知名（方便补表） |
| RA 首次索引慢，首屏无色 | base 层（syntect）兜底；可选在状态栏显示 `$/progress`（L3） |
| 全量 `didChange` payload 过大 | 1 MB 以上不发；写线程承担阻塞 |
| 退出挂起 | 各步超时 + kill 兜底（§8） |
| 语义 token 与诊断在同一 overlay 层冲突 | 约定"后注入者优先"，并在 `replace_layer` 文档里写明 |
| 编辑器文本与 RA 看到的文本不一致（外部改动文件后） | 保存/重新加载时 `didOpen` 重置；不做文件监听（v1） |
| CRLF / BOM | `fs::read_text_file` 已处理行尾与 BOM；发给 RA 时用 `lines.join("\n")` 重建（一次分配，低频） |

---

## 12. 验收

**自动化（无需真 rust-analyzer）**

- `transport.rs`：帧编码/解码往返；粘包（一次读到 1.5 个帧）与拆包（一个帧分多次到）；`Content-Length` 大小写
- `protocol.rs`：`initialize` / `didOpen` / `didChange` / `semanticTokens` 请求 JSON 快照（golden 测试）；响应解析
- delta 解码：§11 列出的四类边界
- `uri.rs`：§6.1 的单测清单
- `theme.rs`：legend 顺序变化时颜色不错位（构造两个不同顺序的 legend，断言同一名字得到同一 Style）
- 降级：模拟 `Exited` 事件 → overlay 清空、base 仍在、编辑器不崩

**集成（可选，`#[ignore]`）**

- PATH 里有 `rust-analyzer` 时跑真实握手：打开本仓库的 `src/main.rs`，断言 overlay 层非空且第 1 行 `use` 着 keyword 色

**手动**

- 输入时颜色不闪、不整体漂移；
- 杀掉 rust-analyzer 进程后编辑器继续可用并提示一次；
- `I_EDIT_LSP=off` 时行为与接入前完全一致。

---

## 附录 A：消息流时序

| 时刻 | 方向 | 消息 |
| --- | --- | --- |
| 打开 `foo.rs` | → | spawn `rust-analyzer` |
| | → | `initialize`（声明 semanticTokens 能力 + 我们认识的 token 名） |
| | ← | `InitializeResult`（含 `legend`）→ 预计算 `Vec<Style>` |
| | → | `initialized` |
| | → | `textDocument/didOpen`（uri, languageId=rust, version=1, text） |
| | → | `textDocument/semanticTokens/full`（记 version=1） |
| | ← | `SemanticTokens { resultId, data }` → 校验 version → `replace_layer(Overlay, rows)` |
| 输入字符 | — | `version += 1`；overlay 水位失效；base 层（syntect）顶上 |
| 静止 150 ms | → | `didChange`（全量，version=n） |
| | → | `semanticTokens/full`（记 version=n） |
| | ← | 响应 → 校验 → 注入 |
| 切到 `bar.md` | → | `textDocument/didClose`（foo.rs） |
| | — | overlay 清空，只留 base |
| 退出 | → | `shutdown` → `exit` → `wait`/`kill` |

## 附录 B：改动点清单

| 文件 | 位置 | 改动 |
| --- | --- | --- |
| `src/lsp/transport.rs` | 新增 | 子进程、读写线程、帧编解码、pending 表、生命周期与超时 |
| `src/lsp/protocol.rs` | 新增 | 5 个方法的请求构造 / 响应解析（含最小结构体定义） |
| `src/lsp/theme.rs` | 新增 | legend 名字 → `Vec<Style>` / `Vec<Modifier>` 预计算 + 默认表 |
| `src/lsp/uri.rs` | 新增 | `file://` 构造（含 Windows 盘符与 percent-encoding） |
| `src/lsp.rs` | 新增 | 模块聚合 + `LspClient` 外观（供 `App` 调用） |
| `src/app.rs` | `92-144` | `drain_incoming()` 里处理 `LspEvent`、防抖请求、退出时关闭 |
| `src/app.rs` | `285-300`、`331-346` | 打开 / 保存后 `didOpen`、版本推进 |
| `src/widgets/editor.rs` | `191-204`、`242-292` | version 与失效（color-rendering-plan Phase 2 已含） |
| `src/widgets/status_bar.rs` | `142-164` | L3：可选显示索引进度 |
| `src/highlight.rs` | — | 不改（能力已具备，只被喂数据） |
| `src/widgets/viewport.rs` | — | 不改 |
| `src/text.rs`、`src/fs.rs` | — | 不改 |
