# i-edit 多色渲染能力方案（着色层）

> 状态：草案 v1 ｜ 日期：2026-10-06 ｜ 基于当前工作区源码的静态分析，行号以当时版本为准

## 0. 定位：这不是语法高亮方案

本文档只解决一件事：**让编辑器具备"把一行文本按区间渲染成不同颜色"的能力**。

- 着色层**自己不产生任何颜色**。它只接受"第 y 行的 [字节区间) 用这个 `Style`"这样的输入，然后画出来。
- 颜色从哪来、代表什么语义，由**外部生产者**决定：
  - 今天：测试代码、一个可开关的演示源；
  - 明天：rust-analyzer 的 `textDocument/semanticTokens`、诊断波浪线、当前行背景、选区与搜索命中。
- 因此本文档**不设计**任何词法规则、关键字表、语言识别。那是 [语法层方案](./syntax-highlight-plan.md) 的事——它将来只是本文 §3 中 `base` 层的一个可选实现。

**一句话**：渲染层只认 `(字节区间, Style)`；一切语义都在渲染层之外解析成 `Style`。这是能接住 rust-analyzer 的前提——语义 token 的 token type / modifier 由服务端 legend 决定，渲染层若掺和进去就会被服务端的能力表绑架。

### 0.1 约束的边界（别把我的假设当成约束）

| 项 | 是否硬约束 | 依据 |
| --- | --- | --- |
| 渲染 / 按键路径零堆分配 | **是** | `allocation-plan.md` 与 `tests/alloc_counting.rs`，项目既有度量体系 |
| 关闭着色时输出与现状逐格等价 | **是** | 防止基础设施改动引入静默回归 |
| 依赖数量 / 依赖体积 | **否** | 项目目前依赖少是结果，不是规定。**新增依赖由使用者决定，本文档不替你否决** |

---

## 目录

- [1. 现状](#1-现状)
- [2. 从 rust-analyzer 反推的需求](#2-从-rust-analyzer-反推的需求)
- [3. 数据模型](#3-数据模型)
- [4. 渲染](#4-渲染)
- [5. 失效、版本与陈旧数据](#5-失效版本与陈旧数据)
- [6. 注入接口（生产者契约）](#6-注入接口生产者契约)
- [7. 主循环改造](#7-主循环改造)
- [8. 分配政策与验收](#8-分配政策与验收)
- [9. 分阶段实施](#9-分阶段实施)
- [10. 风险与回归](#10-风险与回归)
- [附录 A：API 骨架](#附录-aapi-骨架)
- [附录 B：接 rust-analyzer 时还需决定的事](#附录-b接-rust-analyzer-时还需决定的事)
- [附录 C：改动点清单](#附录-c改动点清单)

---

## 1. 现状

| 事实 | 位置 | 对本次改造的影响 |
| --- | --- | --- |
| 文本逐字符写入 `Buffer`，**从不 `set_style`** | `viewport.rs:105-118` | 唯一的着色注入点就在这里 |
| 唯一的颜色是行号槽（DarkGray / White）与欢迎屏 | `viewport.rs:13-14`、`editor.rs:38-39` | 现有断言依赖它们，不能被动到 |
| 文档是 `Vec<String>`，光标 `x` 是**字节偏移** | `text.rs:19-22` | 字节是内部通用坐标，着色沿用即可 |
| 主循环 `event::read()` **阻塞** | `app.rs:92-144` | 异步来源（未来的 LSP）无法触发重绘，必须先改造（§7） |
| 无文档版本概念 | `editor.rs:191-204` | 异步数据会陈旧，需要 version（§5） |
| 每帧 / 每按键零堆分配是硬约束 | `allocation-plan.md` | 着色是每帧路径，必须 0 分配（§8） |

---

## 2. 从 rust-analyzer 反推的需求

LSP `textDocument/semanticTokens` 的几个硬事实，直接决定了数据模型怎么设计：

| LSP 事实 | 对设计的约束 |
| --- | --- |
| 坐标单位是 **UTF-16 码元**，不是字节、不是显示列 | 内部不能用 UTF-16；必须在**注入时一次性转换**，渲染时零转换（§3.4） |
| 返回 `data: Vec<u32>` 是 **delta 编码**（`delta_line, delta_start, length, type, modifiers`） | 解码一次性完成；同行的 token 天然按位置递增，渲染游标要求有序，正好吻合 |
| token type / modifier 是**服务端 legend 的下标**，legend 由 `initialize` 给出 | 渲染层不能认识 "keyword"/"struct"；只认识 `Style`。名字→`Style` 的映射表放在生产者侧 |
| **异步**到达，且可能针对旧版本计算 | 需要文档 version + 陈旧丢弃策略（§5） |
| 未覆盖的区间应保持"无颜色" | 需要分层回退：`overlay` 没盖住的地方回退 `base`，都没有就默认（§3.2） |
| 诊断（`publishDiagnostics`）要**下划线**与背景 | `Style` 必须能带 `Modifier::UNDERLINED` 与 `underline_color`、`bg`——所以 run 里存完整 `Style`，而不是"颜色 id"或"token 种类" |
| 诊断区间可能是**零长度**（行尾） | run 需允许 `start == end`，渲染时至少画一格（§10） |

结论：**run 里存 `Style`，区间用字节偏移。** 这两条是全部设计的支点。

---

## 3. 数据模型

新模块 `src/highlight.rs`。

### 3.1 `StyledRun`

```rust
/// 一行的着色指令：行内字节区间 `[start, end)` 染成 `style`。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StyledRun {
    pub start: u32,   // 行内字节偏移
    pub end: u32,     // 开区间右端
    pub style: Style, // ratatui Style 是 Copy：含 fg / bg / underline_color / modifier
}
```

为什么是**完整 `Style` 而不是 `TokenKind`**：

- rust-analyzer 的 token type × modifier 组合是二维的（`async`+`reference`+`readonly`…），只有 `Style` 能无损承载；
- 诊断需要下划线颜色，选区需要背景，都不是"前景色"能表达的；
- 渲染层因此完全不需要知道语义，换生产者不用改渲染。

### 3.2 两层：`base` + `overlay`

```rust
pub struct Highlights {
    base: Layer,     // 语法层（本地扫描，未来可选）；低频刷新
    overlay: Layer,  // 语义/诊断/选区（rust-analyzer）；增量刷新
    enabled: bool,
}
```

- 解析规则：`overlay` 盖住的字节用 `overlay`，否则 `base`，否则默认样式。
- **不做合并**：渲染时用两个游标同时推进，`overlay` 优先。合并就要每帧产出第三个容器 —— 那是每帧分配，不行。
- 一层可以整层为空（`enabled = false` 或生产者还没给数据），此时行为与今天一致。

### 3.3 `Layer`：扁平竞技场 + 每行的区间索引

```rust
pub struct Layer {
    arena: Vec<StyledRun>,  // 所有行的数据，按行分段连续存放
    lines: Vec<LineSpan>,   // 每行在 arena 中的 [start, end)，长度 = 文档行数
    valid_upto: usize,      // 行级水位：lines[..valid_upto] 与当前文本一致
}

#[derive(Debug, Clone, Copy, Default)]
struct LineSpan { start: u32, end: u32 }
```

- 为什么不是 `Vec<Vec<StyledRun>>`：per-line 小 Vec 意味着每行一次分配、缓存不友好；竞技场是 2 个分配，且注入语义 token 时本来就是"整份替换"，重建成本一样。
- 插入/删除行：`lines.splice()`，8 字节元素 memcpy，可忽略。
- `valid_upto` 与 `arena` 的关系：水位只标记"行索引"层面的可信度；`arena` 里陈旧段由 `lines` 的区间定位，失效时把 `lines[i].start = lines[i].end`（置空）即可，不必动 `arena`。

### 3.4 坐标转换（UTF-16 → 字节）

```rust
/// LSP 的行内列号（UTF-16 码元）→ 行内字节偏移。
///
/// 只在注入时调用（每 token 一次），渲染路径永不调用。
pub fn utf16_to_byte(line: &str, col16: usize) -> usize {
    let mut units = 0;
    for (offset, ch) in line.char_indices() {
        if units >= col16 {
            return offset;
        }
        units += ch.len_utf16();
    }
    line.len()
}
```

反向（`byte_to_utf16`）在采用**全量同步**（`didChange` 发整篇文本）时不需要，v1 不做；等将来要做增量同步再补（见附录 B）。

### 3.5 归属

`Highlights` 放进 **`ViewportState`**（`viewport.rs:22`），与上一版方案同理：`StatefulWidget::render` 已经拿到 `&mut ViewportState`，渲染期需要可变访问（推进游标、按水位补数据都由渲染驱动），无需新增任何借用；`Viewport::new()` 签名不变，`editor.rs:95` 不动。

文档 version、失效通知由 `EditorState` 持有并转发（`editor.rs:242-292`）。

---

## 4. 渲染

改动集中在 `viewport.rs:105-118`。核心是**双游标**：

```rust
let (base, overlay) = state.highlights.line_runs(absolute_y);

let mut bi = 0usize;  // base 游标
let mut oi = 0usize;  // overlay 游标
let mut col_offset = -(state.scroll_x as i32);

for (byte_idx, ch) in line.char_indices() {
    let char_width = UnicodeWidthChar::width(ch).unwrap_or(0) as i32;

    while bi < base.len() && byte_idx >= base[bi].end as usize { bi += 1; }
    while oi < overlay.len() && byte_idx >= overlay[oi].end as usize { oi += 1; }

    // overlay 优先，其次 base，最后默认
    let style = overlay.get(oi)
        .filter(|r| (r.start as usize) <= byte_idx)
        .or_else(|| base.get(bi).filter(|r| (r.start as usize) <= byte_idx))
        .map_or(Style::default(), |r| r.style);

    if col_offset >= 0 && col_offset + char_width <= text_width {
        let x = area.x + content_offset_x as u16 + col_offset as u16;
        let cell = &mut buf[(x, area.y + relative_y as u16)];
        cell.set_char(ch).set_style(style);
        if char_width == 2 {
            // 宽字符第二格必须同色，否则中文注释/字符串出现色块断裂
            buf[(x + 1, area.y + relative_y as u16)].set_char(' ').set_style(style);
        }
    }

    col_offset += char_width;
}
```

要点：

1. **游标只按字节推进**，与 `scroll_x`（显示列）无关，横向滚动不会让颜色错位。
2. `Style::default()` 与 `Buffer` 未写入单元格的样式相同 → `enabled = false` 或空层时输出与今天**逐字节等价**（§8 的 S5 闸门）。
3. **宽字符第二格同步设色**（`viewport.rs:112-114` 现有分支）。
4. **背景色要铺到行尾**：若某行最后一个 run 带 `bg` 且 `end == line.len()`，需把该行剩余可见列（到 `text_width`）也填成该背景——否则"当前行高亮""诊断背景"会在行尾断开。这是一条独立的小逻辑，放在字符循环之后：

   ```rust
   // 只有存在背景 run 时才走这段，普通文本零成本
   if let Some(bg) = trailing_bg(base, overlay, line.len()) { /* 填到 text_width */ }
   ```

5. 行号槽（`viewport.rs:86-100`）不动。

---

## 5. 失效、版本与陈旧数据

### 5.1 文档版本

`EditorState` 增 `version: u64`：`handle_key` 里任何编辑操作后 `version += 1`；`load_file` 归零（或继续累加，只要单调）。用途：

- 生产者请求颜色时带上当时版本；
- 回来的数据带版本，版本不匹配 → 丢弃（rust-analyzer 很快，重算比纠错便宜）。

### 5.2 失效通知（O(1)）

```rust
// editor.rs: handle_key 末尾
if edited {
    self.dirty = true;
    self.version += 1;
    self.viewport_state.highlights.note_edit(
        y_before.min(self.text.cursor.y),
        self.text.lines.len() != lines_before,   // Enter / 行首退格会改变行数
    );
}
```

`note_edit` 只做两件事：把水位压到 `line`，并把 `lines[line..]` 的区间置空（不清 `arena`，容量复用）。**不重算、不分配。**

`load_file` 调 `highlights.reset(line_count)`（1 次分配，重建 `lines` 索引）。

### 5.3 陈旧数据

语义 token 是"尽力而为"的装饰：编辑后到新数据到达之前，缺失处回退 `base` 层或无色。用户看到的是"刚敲的那行暂时没色"——可接受，且比显示错色好。**不做**任何基于编辑差分的 token 平移（那是 rust-analyzer 增量同步阶段才值得做的事）。

---

## 6. 注入接口（生产者契约）

渲染是**拉模型**（渲染时问 `line_runs(y)`），注入是**推模型**（数据到达时整份写入）。两者分离，是为了让"数据来自另一个线程"这件事不影响渲染路径。

```rust
impl Highlights {
    /// 渲染路径专用：只读，零分配。
    pub fn line_runs(&self, y: usize) -> (&[StyledRun], &[StyledRun]);

    /// 注入路径专用：允许分配，只在数据到达 / 文件加载时调用。
    /// `rows` 必须按行号升序给出，每行内的 run 必须按 start 升序。
    pub fn replace_layer(&mut self, which: LayerId, rows: &[Vec<StyledRun>]);
    pub fn clear_layer(&mut self, which: LayerId);
}
```

`replace_layer` 的实现要点：

- `arena.clear()` 后顺序 extend，`lines` 就地重写（`Vec::clear` + push，容量复用）→ 稳态下第二次响应开始就不再增长；
- 调用方（生产者）自己持有构建缓冲，避免每响应新建 `Vec<Vec<_>>`；
- 断言（debug 下）：每行 run 的 `end` 单调递增、`end <= line.len()`。

**生产者清单（都是本文档之外的东西）**

| 生产者 | 层 | 何时 | 本文档是否实现 |
| --- | --- | --- | --- |
| 演示源（验证链路） | overlay | 命令触发 | 是（Phase 3，可删） |
| 单元测试直接注入 | 任意 | 测试内 | 是 |
| 本地语法扫描器 | base | 打开 / 编辑后 | 否，见 `syntax-highlight-plan.md` |
| rust-analyzer 语义 token | overlay | 异步响应 | 否，下一份方案 |
| 诊断波浪线 | overlay | 异步通知 | 否 |
| 当前行背景 / 选区 / 搜索命中 | overlay | 同步 | 否（能力已具备） |

---

## 7. 主循环改造

**这是接 rust-analyzer 之前必须动的一刀**，现在先做掉，成本极低。

现状（`app.rs:92-144`）：`draw` → `event::read()`（无限阻塞）。阻塞期间收不到任何异步数据，也不会重绘。

改造后：

```rust
const POLL: Duration = Duration::from_millis(50);

loop {
    self.drain_incoming();          // 今天：空实现；将来：LSP / 外部变更
    if self.needs_redraw {
        terminal.draw(|frame| self.render(frame))?;
        self.needs_redraw = false;
    }
    if !event::poll(POLL)? {
        continue;                   // 超时：回到顶部，给 drain 一次机会
    }
    let event = event::read()?;
    /* ... 现有分发 ... */
    self.needs_redraw = true;
}
```

- `needs_redraw` 初始为 `true`（首帧必画）；处理完事件、或 `drain_incoming()` 拿到东西时置 `true`。
- 空闲时 50 ms 一次循环 ≈ 20 次/秒的空转，无绘制时几乎零成本。
- `drain_incoming()` 本期是空函数 + 注释占位，把"异步入口"这个位置先占住。

---

## 8. 分配政策与验收

沿用 `tests/alloc_counting.rs` 的 `measure()`（`tests/alloc_counting.rs:93`）。

| # | 测点 | 目标 |
| --- | --- | --- |
| C1 | 1k 行、每行 3 个 run（两层都有）渲染一帧 | **0** |
| C2 | 同上，输入 100 个字符 | ≤ 1（摊销），且每次按键只做 O(1) 失效 |
| C3 | `replace_layer` 第二次调用（容量已建立） | **0** |
| C4 | `replace_layer` 首次调用 | 1（`arena`）+ 1（`lines`），低频，接受 |
| C5 | `enabled = false` 渲染 1k 行 | **0**，数值与现状 `2a` 相同 |
| C6 | 冷启动首帧（无颜色） | ≤ 现状 `2b` 数值，不得变高 |

正确性测试（`src/highlight.rs` + `viewport.rs`）：

- 单层 / 双层叠加：`overlay` 覆盖处胜出，未覆盖处回退 `base`；
- `scroll_x > 0` 时颜色与字符不错位；
- 宽字符（中文、emoji）第二格同色；
- 带 `bg` 的 run 铺到行尾；
- `utf16_to_byte`：ASCII / BMP 中文 / 含代理对（emoji）三种输入的对照表；
- 零长度 run（`start == end`）不 panic、不吞掉后续字符；
- 编辑后水位失效：改第 5 行后第 5 行及其后颜色清空，前 4 行保留；
- **C5 的硬验证**：`enabled = false` 时对 1k 行渲染输出做 symbol 串 + 样式串快照，与改造前逐格相同。

---

## 9. 分阶段实施

| Phase | 内容 | 文件 | 行数 |
| --- | --- | --- | --- |
| 0 | `highlight.rs` 骨架：`StyledRun` / `LineSpan` / `Layer` / `Highlights` / `utf16_to_byte` / `line_runs` / `replace_layer` / `note_edit` + 单测 | `src/highlight.rs`（新）、`src/lib.rs` | ~200 |
| 1 | 顶点着色：`ViewportState` 持 `Highlights`；双游标 + `set_style`；宽字符第二格；bg 铺行尾 | `src/widgets/viewport.rs` | ~50 |
| 2 | 失效与版本：`EditorState` 加 `version`、`note_edit`、`load_file` reset | `src/widgets/editor.rs` | ~25 |
| 3 | 演示源：命令面板 `paint demo`（临时命令，接 LSP 后可删）+ 渲染断言测试 | `src/widgets/command.rs`、`src/action.rs`、`src/app.rs` | ~40 |
| 4 | 主循环：`event::poll` + `needs_redraw` + `drain_incoming()` 占位 | `src/app.rs` | ~25 |
| 5 | 测试固化：C1–C6 + 关闭态快照闸门 | `tests/alloc_counting.rs` | ~120 |

**提交切分**

1. `feat: styled run storage and line overlay API`
2. `feat: paint viewport text from styled runs`
3. `feat: document version and highlight invalidation`
4. `feat: demo paint source to prove the pipeline`
5. `refactor: poll-based main loop with an async intake hook`
6. `test: allocation and snapshot guards for colored rendering`

Phase 5 可与 4 合一个提交（都是主循环），但建议分开：主循环行为变化风险更高，独立可回滚。

**本期不做**：任何词法/语法分析；rust-analyzer 客户端；主题表；语言识别；配置解析。

---

## 10. 风险与回归

| 风险 | 对策 |
| --- | --- |
| 双游标逻辑写错导致颜色整体偏移一格 | 边界测试：`start == 0`、`start == line.len()-1`、相邻 run 首尾相接 |
| 背景色未铺行尾导致半截色块 | 单独测试 + 只在存在 bg run 时执行该分支（普通行零成本） |
| 零长度 run（诊断行尾）吞字符或 panic | 渲染时 `start == end` 视为"覆盖到行尾一格"，且不影响后续游标推进 |
| UTF-16 转换在代理对处出错 | 三档对照测试：纯 ASCII / 中文（BMP）/ emoji（代理对） |
| 陈旧 token 与文本错位 | 版本不匹配即丢弃；水位失效后整段置空，宁可无色 |
| `ViewportState` 加字段破坏既有测试 | 现有测试用 `..Default::default()` 构造（`viewport.rs:142,158`），`Highlights` 实现 `Default` 即可 |
| 关闭态行为漂移 | C5 + 快照测试（`fg`/`bg`/`modifier` 逐格比对），是硬闸门 |
| 主循环改 poll 后丢事件 / 空转烧 CPU | poll 超时 50 ms；单测覆盖"无事件时不重绘" |
| 未来 inlay hints（虚拟文本）无法用 run 表达 | 明确超出本能力范围：那需要在 viewport 里插入"不来自 buffer 的字符"，届时需另开方案；本设计不阻塞它 |

---

## 附录 A：API 骨架

```rust
// src/highlight.rs
use ratatui::style::Style;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StyledRun { pub start: u32, pub end: u32, pub style: Style }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerId { Base, Overlay }

#[derive(Debug, Default)]
pub struct Highlights { base: Layer, overlay: Layer, enabled: bool }

impl Highlights {
    pub fn enabled(&self) -> bool;
    pub fn set_enabled(&mut self, on: bool);
    pub fn line_runs(&self, y: usize) -> (&[StyledRun], &[StyledRun]);      // 渲染，0 分配
    pub fn replace_layer(&mut self, which: LayerId, rows: &[Vec<StyledRun>]); // 注入，允许分配
    pub fn clear_layer(&mut self, which: LayerId);
    pub fn note_edit(&mut self, line: usize, line_count_changed: bool);      // O(1)
    pub fn reset(&mut self, line_count: usize);
}

pub fn utf16_to_byte(line: &str, col16: usize) -> usize;
```

**未来 rust-analyzer 注入侧的伪代码**（不属于本期实现，仅验证接口够用）：

```rust
// 一次性解码 delta，产出每行的 runs；允许分配
let mut line = 0usize;
let mut start16 = 0usize;
let mut rows: Vec<Vec<StyledRun>> = Vec::with_capacity(lines.len());
for chunk in data.chunks(5) {
    line += chunk[0] as usize;
    start16 = if chunk[0] == 0 { start16 + chunk[1] as usize } else { chunk[1] as usize };
    let end16 = start16 + chunk[2] as usize;
    let style = theme.style_for(legend_type(chunk[3]), chunk[4]); // 名字 → Style，在生产者侧
    let start = utf16_to_byte(&lines[line], start16);
    let end = utf16_to_byte(&lines[line], end16);
    if start < end { rows[line].push(StyledRun { start: start as u32, end: end as u32, style }); }
}
highlights.replace_layer(LayerId::Overlay, &rows);
```

## 附录 B：接 rust-analyzer 时还需决定的事

> 已另立 [`rust-analyzer-plan.md`](./rust-analyzer-plan.md)，本附录只留决策索引。

| 议题 | 选项 | 备注 |
| --- | --- | --- |
| 传输 | stdio 子进程（`rust-analyzer` 可执行文件） | 本期 `drain_incoming()` 的占位就是为它留的 |
| JSON 编解码 | `serde_json` + 手写最小结构体（建议）vs `lsp-types` 全套 | 依赖本身不是否决项。建议只引 `serde_json`：LSP 面很大，但我们要用的只有 initialize/didOpen/didChange/semanticTokens/publishDiagnostics 五个方法，`lsp-types` 会连带 `url`/`serde_repr`/`serde` 一整条链；等哪天要做 codeAction / inlayHints / completion 再换成 `lsp-types` 更划算 |
| 同步方式 | 全量 `didChange`（简单）vs 增量（需计算 range diff + UTF-16 位置） | 个人用途建议全量；本期已提供 `utf16_to_byte`，增量方向未被堵死 |
| 请求时机 | 编辑静止 ~150 ms 后请求（防抖） | 需要 `Instant` + 在主循环里检查，本期 poll 循环已具备条件 |
| 生命周期 | 按 `Language == Rust` 启停；打开/关闭文件时 `didOpen`/`didClose` | 需要"当前语言"概念，语法层方案里有 `Language::from_path` |
| 主题 | legend 名字 → `Style` 的映射表 + `I_EDIT_COLOR=ansi16\|256\|truecolor` | 参照 `main.rs:11-14` 的 `I_EDIT_LOG` 约定 |
| 诊断 | 区间 → `Style`（下划线 + `underline_color`），零长度区间画行尾一格 | 本期 run 模型已能承载 |

## 附录 C：改动点清单

| 文件 | 位置 | 改动 |
| --- | --- | --- |
| `src/highlight.rs` | 新增 | `StyledRun` / `LineSpan` / `Layer` / `Highlights` / `utf16_to_byte` + 单测 |
| `src/lib.rs` | `1-11` | `pub mod highlight;` |
| `src/widgets/viewport.rs` | `22-32` | `ViewportState` 增 `highlights: Highlights`（`pub(crate)`） |
| `src/widgets/viewport.rs` | `105-118` | 双游标 + `set_style`；宽字符第二格；bg 铺行尾 |
| `src/widgets/viewport.rs` | `123+` | 着色渲染测试（叠加 / 滚动 / 宽字符 / 关闭态） |
| `src/widgets/editor.rs` | `191-204` | `EditorState` 增 `version: u64` |
| `src/widgets/editor.rs` | `242-292` | `handle_key` 记 `y_before` / `lines_before`，编辑后 `version += 1` + `note_edit` |
| `src/widgets/editor.rs` | `224-235` | `load_file` 调 `highlights.reset()` |
| `src/app.rs` | `92-144` | 主循环：`event::poll` + `needs_redraw` + `drain_incoming()` 占位 |
| `src/app.rs` | `173-216` | Phase 3：`Action::PaintDemo` 分支（演示源，可删） |
| `src/action.rs` | `12-37` | Phase 3：`PaintDemo` 变体 |
| `src/widgets/command.rs` | `87-96` | Phase 3：命令表加 `paint demo` 一行 |
| `tests/alloc_counting.rs` | 新增 | C1–C6 |
| `src/fs.rs`、`src/text.rs` | — | 不改 |
