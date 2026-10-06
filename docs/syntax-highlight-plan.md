# i-edit 语法高亮方案

> 状态：草案 v1 ｜ 日期：2026-10-06 ｜ 基于当前工作区源码的静态分析，行号以当时版本为准
>
> **范围澄清（2026-10-06）**：本文只覆盖"本地语法扫描器"，它是
> [`color-rendering-plan.md`](./color-rendering-plan.md) 中 `base` 层的一个**可选实现**。
> 颜色渲染能力本身（按区间着色、`base`/`overlay` 分层、注入接口、异步入口）在那份文档里，
> 且不依赖本文。若先接 rust-analyzer，本文可整体延后。
>
> **前提修正（2026-10-06）**：初版把"零依赖"当成否决 syntect / tree-sitter 的理由，
> 这是本文档的假设，不是项目约束——项目真正立过的目标只有"零分配"（`allocation-plan.md`）。
> 依赖数量由使用者决定。§2 已按这个前提重做选型：**推荐改为 syntect**，
> §3–§5 与附录 A 的手写扫描器设计降为"无资源依赖"的备选路径（候选 A）。

**与既有文档的关系**：本方案遵循 [`allocation-plan.md`](./allocation-plan.md) 确立的分配政策——每帧 / 每按键路径上由本项目代码触发的堆分配必须为 0，I/O 与低频路径只做必需量。高亮是**每帧路径**上的新功能，因此"零分配"是硬约束，不是优化项。

## 目录

- [1. 背景与目标](#1-背景与目标)
- [2. 技术选型](#2-技术选型)
- [3. 数据模型](#3-数据模型)
- [4. 渲染路径改造](#4-渲染路径改造)
- [5. 失效与增量](#5-失效与增量)
- [6. 语言识别](#6-语言识别)
- [7. 主题与配色](#7-主题与配色)
- [8. 外壳集成](#8-外壳集成)
- [9. 分配政策与验收](#9-分配政策与验收)
- [10. 分阶段实施](#10-分阶段实施)
- [11. 风险与回归](#11-风险与回归)
- [附录 A：首版词法规则](#附录-a首版词法规则)
- [附录 B：改动点清单](#附录-b改动点清单)

---

## 1. 背景与目标

### 1.1 现状

编辑器目前是**纯单色**的：

- `Viewport::render`（`src/widgets/viewport.rs:105-118`）逐字符写 `buf[(x, y)].set_char(ch)`，**从不调用 `set_style`**；
- 唯一的颜色是行号槽：`LINE_NUMBER_STYLE` / `ACTIVE_LINE_NUMBER_STYLE`（`viewport.rs:13-14`，DarkGray / White）与欢迎屏的 `TITLE_STYLE` / `HINT_STYLE`；
- 文本由 `Vec<String>` 按行持有（`TextState::lines`），渲染直接借用，无中间表示。

即：文本 → 单元格是一条直线，没有可以挂颜色的环节。

### 1.2 目标

| 目标 | 说明 |
| --- | --- |
| G1 语法着色 | 按语言给关键字 / 字符串 / 数字 / 注释 / 类型 / 函数等着色 |
| G2 每帧零分配 | 高亮开启后，稳态渲染一帧的本项目堆分配 = 0（与现状 `2a` 测点一致） |
| G3 每按键零分配 | 输入字符后只做 O(1) 失效标记，重算推迟到下一帧且只覆盖可见行 |
| G4 关闭时逐字节等价 | 高亮关闭时的渲染输出与今天完全相同（不只是"看起来一样"） |
| G5 可扩展 | 加一门语言 = 加一张关键字表 + 一个扫描分支；主题可换 |

**非目标**：LSP / 语义高亮、括号匹配高亮、选区与搜索高亮（但数据模型预留位置，见 §3.2）、主题配置文件解析。

**不是约束**：依赖数量与体积。是否引入 syntect / tree-sitter 是成本决策，不由本文档代劳（见 §2）。

---

## 2. 技术选型

### 2.1 先回答一个前置问题：有了 rust-analyzer，还需要本地语法层吗？

| 场景 | rust-analyzer 语义 token 能覆盖吗 | 结论 |
| --- | --- | --- |
| Rust 文件，RA 已就绪 | 能，且比任何本地扫描器准（它知道 `foo` 是宏还是函数、类型还是变量） | 不需要本地层 |
| Rust 文件，RA 未启动 / 崩溃 / 索引中（首屏几秒） | 不能 | 需要兜底 |
| 非 Rust 文件（`md` / `toml` / `json` / `sh` …） | 不能（除非再接一个对应 LSP） | 需要本地层 |

所以本地层的定位是：**RA 未就绪时的兜底 + 非 Rust 文件的唯一着色来源**。它是"够用就行"的层，不值得为它手写 7 门语言的词法。

### 2.2 候选对比（依赖不再是否决项）

| 方案 | 准确度 | 依赖成本 | 语言覆盖 | 分配代价（关键） |
| --- | --- | --- | --- | --- |
| **B. syntect**（TextMate 语法，推荐） | 高（正则 + 上下文栈，接近编辑器水平） | `syntect` + 语法资源（内置 `default-syntaxes` 二进制约 1–2 MB；也可外置 `.sublime-syntax` 文件按需加载） | 100+，开箱即用 | 无增量：每次改文本需重解析。但**重解析发生在注入侧**（`replace_layer`，防抖后每 ~150 ms 一次），渲染路径只读 arena，仍然 0 分配 |
| C. tree-sitter | 高（语法树） | C 源码，Windows 需 cc 工具链；每语言一个 grammar crate | 每语言单独引 | 原生增量；但仍需把节点映射成行区间再喂给注入接口 |
| A. 自研行扫描器 | 中（纯词法，无语法上下文） | 0 | 手写一门约 40–80 行 | 渲染路径 0；每文件 1 个 `Vec<u8>` |

**结论：推荐 B（syntect）。** 理由：

1. 依赖不是约束后，B 的"每行/每语言都要手写"是最贵的成本——7 门语言约 300–500 行且要长期维护、逐语言调边界；syntect 是零成本拿 100+ 语言。
2. 分配政策**不被破坏**：G2/G3 约束的是渲染与按键路径。syntect 的重解析只发生在"文本变了之后的防抖时刻"，走的是 `color-rendering-plan.md` §6 的推模型注入接口，允许分配；渲染侧 `line_runs()` 依然零分配。
3. 兜底层不需要多准：RA 一起来，`overlay` 层就盖住它了。

**采用 B 时的工作面**（比手写小得多）：

- `syntax.rs` 只做三件事：加载语法集与主题（`SyntaxSet`、`ThemeSet`，`OnceLock` 懒加载一次）、按 `Language::from_path` 选 `SyntaxReference`、把 syntect 的 `(range, Style)` 流**转成字节区间**喂给 `Highlights::replace_layer`。
- 关键转换：syntect 的 `HighlightIterator` 产出的是**字符/字节区间 + `StyleChange`**，要按行切开（`\n` 处换行），并把 syntect 的 `Color { r,g,b,a }` 映射成 `ratatui::style::Color::Rgb`（或按 `I_EDIT_COLOR` 降级到 256/16 色）。
- 行切分是必须的：着色层的数据模型是"每行的 run 列表"，而 syntect 是流式产出。做法：按行迭代，`HighlightIterator` 按行推进（或整文件跑一遍后按 `\n` 分段）。

**保留 A 的场景**：若最终决定不引任何新依赖（例如要控制二进制体积 / 离线构建），§3–§5 与附录 A 的手写设计是完整可执行的备选，直接照做即可。

**退出通道**：无论选哪个，对外只暴露"给我第 y 行的 run 列表"，渲染层不动。

---

## 3. 数据模型

新模块 `src/syntax.rs`（单文件起步，语言变多后拆 `src/syntax/` 目录 + `langs.rs`）。

### 3.1 核心类型

```rust
/// 一段同色文本，只记结束字节偏移。
///
/// 不记起点：渲染时用游标推进，省一个字段也省对齐填充；`u32` 足够——
/// 单文件上限 10 MiB（`fs::MAX_FILE_SIZE`）。
#[derive(Debug, Clone, Copy)]
pub struct Run {
    pub end: u32,      // 字节偏移，开区间右端
    pub kind: TokenKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TokenKind {
    Plain = 0,
    Comment, String, Number, Keyword, Type, Function,
    Macro, Attribute, Operator, Punctuation, Property,
    Heading, Link,                 // Markdown
    // 100.. 预留给"叠加层"：选区 / 搜索命中，见 §11.6
    Selection = 100, SearchMatch = 101,
}

/// 行首状态：跨行结构的唯一入口。Copy，2 字节，按行存一个。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LineState {
    block: Block,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Block { #[default] None, Comment(u8), Str(u8), Fence(u8) }
// Comment/Rust 块注释嵌套深度；Str = 三引号/长字符串分隔符长度；Fence = ``` 的长度
```

```rust
/// 一门语言：关键字表 + 词法开关 + 扫描入口。全部是 `'static` 数据，Copy。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Language {
    #[default] Plain,
    Rust, Markdown, Toml, Json, Yaml, Shell, Python,
    // 表已留位，v1 先落 Plain：JavaScript, TypeScript, C, Cpp, Html, Css, Sql, Go
}
```

### 3.2 高亮器

```rust
pub struct Highlighter {
    language: Language,
    theme: Theme,
    enabled: bool,
    /// 每行行首状态；只有 `states[..valid_upto]` 与当前文本一致。
    states: Vec<LineState>,
    valid_upto: usize,
    /// 渲染时逐行复用的输出槽：clear + extend，容量跨帧、跨行复用。
    runs: Vec<Run>,
}
```

**归属：`Highlighter` 放在 `ViewportState` 里**（`viewport.rs:22`）。

- 理由 1：`StatefulWidget::render` 已经拿到 `&mut ViewportState`，渲染期需要可变访问，放这里无需新增任何借用；
- 理由 2：失效通知与滚动同源（`EditorState` 持有 `viewport_state`，`handle_key` 里一行就能通知）；
- 备选：放 `EditorState` 并在 `editor.rs:95` 用"字段级不相交借用"传 `&mut`（`&state.text.lines` 与 `&mut state.highlighter` 不冲突，能编译）——可行但更绕，且 `Viewport` 的构造签名会变长。

对外 API（全部零分配，除 `set_language` 重建 `states`）：

| 方法 | 调用点 | 分配 |
| --- | --- | --- |
| `fn runs(&self) -> &[Run]` | `viewport.rs` 渲染循环 | 0 |
| `fn highlight_line(&mut self, lines: &[String], y: usize)` | `viewport.rs` 每行一次 | 0（稳态） |
| `fn invalidate_from(&mut self, line: usize)` | `EditorState::handle_key` 编辑后 | 0 |
| `fn note_edit(&mut self, line: usize, line_count_changed: bool)` | 同上（含行数变化的 Enter/退格合并行） | 0 |
| `fn reset(&mut self, line_count: usize)` | `EditorState::load_file` | 1（`states` Vec） |
| `fn set_language(&mut self, lang: Language)` | 打开 / 另存后 | 1（`states` Vec） |
| `fn toggle(&mut self) -> bool` | `Action::ToggleSyntax` | 0 |

### 3.3 扫描器

扫描器是**纯函数 + 泛型 sink**，避免为"只要状态"和"要 runs"写两遍：

```rust
/// 扫描一行：从 `state_in` 开始，把 token 段写给 `sink`，返回行末状态。
fn scan_line(lang: Language, line: &str, state_in: LineState,
             sink: &mut impl RunSink) -> LineState;

trait RunSink { fn push(&mut self, end: u32, kind: TokenKind); fn len(&self) -> usize; }
impl RunSink for Vec<Run> { /* 超过 MAX_RUNS_PER_LINE 后静默丢弃 */ }
impl RunSink for Discard { /* 只算状态，不产出 */ }
```

- `MAX_RUNS_PER_LINE = 512`：病态长行（例如 5000 个单词）截断后剩余部分回退 `Plain`，**文本仍然完整渲染**（见 §11.2）。
- 关键字匹配：`KEYWORDS: &[&str]`（编译期有序），`KEYWORDS.binary_search(&&line[a..b])` —— 借用切片比较，**零分配**。
- 不做 `to_lowercase`、不 `format!`、不 `collect()`。

---

## 4. 渲染路径改造

改动集中在 `src/widgets/viewport.rs:105-118`（现有文本循环）：

```rust
// 每行一次：算出该行的 token 段（稳态零分配，runs 容量复用）
let runs: &[Run] = if state.highlight.is_active() {
    state.highlight.highlight_line(self.lines, absolute_y);
    state.highlight.runs()
} else {
    &[]
};

let mut run_idx = 0usize;
let mut col_offset = -(state.scroll_x as i32);

for (byte_idx, ch) in line.char_indices() {
    let char_width = UnicodeWidthChar::width(ch).unwrap_or(0) as i32;

    // 游标只按字节推进：与横向滚动无关，scroll_x 不影响对齐
    while run_idx < runs.len() && byte_idx >= runs[run_idx].end as usize {
        run_idx += 1;
    }
    let style = match runs.get(run_idx) {
        Some(run) => state.highlight.style_for(run.kind),
        None => Style::default(),
    };

    if col_offset >= 0 && col_offset + char_width <= text_width {
        let x = area.x + content_offset_x as u16 + col_offset as u16;
        let cell = &mut buf[(x, area.y + relative_y as u16)];
        cell.set_char(ch).set_style(style);
        if char_width == 2 {
            // 宽字符第二格必须同色，否则代码块里的中文/emoji 出现色块断裂
            buf[(x + 1, area.y + relative_y as u16)]
                .set_char(' ')
                .set_style(style);
        }
    }

    col_offset += char_width;
}
```

要点：

1. **`set_style(Style::default())` 与现状等价** —— `Buffer` 未写入的单元格默认 `Style::new()`，因此 `Plain` 写 default 不改变任何现有断言，G4 自动成立。
2. 行号槽（`viewport.rs:86-100`）不动：它已经有自己的两套样式，且高亮不应影响槽位。
3. `Viewport` 的**构造签名不变**（`Viewport::new(lines).active_line(y)`），只是 `render` 内部改读 `state.highlight`；`editor.rs:95` 无需改动。

---

## 5. 失效与增量

不需要缓存每一行的 runs——**每帧重新扫描可见行**（24 行 × ~200 字符 ≈ 5k 次字符判断，< 0.1 ms），换来的是零缓存失效逻辑。真正需要持久化的只有 `LineState`。

### 5.1 水位机制

```
states: Vec<LineState>   // 长度 = 行数
valid_upto: usize        // states[..valid_upto] 可信
```

- `highlight_line(lines, y)` 内部 `ensure_states(lines, y)`：对 `valid_upto..=y` 用 `Discard` sink 只算状态并回填，`valid_upto = y + 1`。跨帧缓存，滚动时补齐。
- 编辑后：`valid_upto = min(valid_upto, y)`（`invalidate_from`），O(1)。
- 行数变化（Enter / 行首退格 / 行尾 Delete）：先 `states.truncate(new_len)` / 按需 push，再 `invalidate_from(y)`。

### 5.2 失效点接入（`editor.rs:242-292`）

```rust
// handle_key 开头记一次，零成本
let y_before = self.text.cursor.y;
let lines_before = self.text.lines.len();

/* ... 现有 match ... */

if edited {
    self.dirty = true;
    self.viewport_state.highlight.note_edit(
        y_before.min(self.text.cursor.y),
        self.text.lines.len() != lines_before,
    );
}
```

`load_file`（`editor.rs:224-235`）末尾：`highlight.reset(lines.len())` + `set_language(Language::from_path(&path))`。

### 5.3 最坏情况与可选优化

在文件第 0 行改动、且该文件第 0 行起有块注释时，向下滚动会触发 `ensure_states` 一直算到可见行末——O(文件行数) 的 u8 扫描（10 万行约 1–2 ms），只在**滚动到那里时**发生一次。若实测不可接受，Phase 6 再加"检查点"：每 512 行缓存一份 `(line_no, LineState)`，失效时只从最近检查点重算。**v1 不做。**

---

## 6. 语言识别

```rust
impl Language {
    /// 扩展名 → 文件名 → shebang，都失败则 Plain。全程借用比较，零分配。
    pub fn from_path(path: &Path) -> Self;
    pub fn from_shebang(first_line: &str) -> Option<Self>;
    pub fn name(self) -> &'static str;   // 供状态栏显示，Plain 返回 ""
}
```

| 优先级 | 规则 | 例 |
| --- | --- | --- |
| 1 | 完整文件名（大小写不敏感） | `Cargo.toml`、`Makefile`/`makefile`、`Dockerfile`、`Justfile` |
| 2 | 扩展名 | `rs` `md` `toml` `json` `yaml`/`yml` `sh`/`bash`/`zsh` `py` |
| 3 | 首行 shebang（无扩展名脚本） | `#!/usr/bin/env python3`、`#!/bin/bash` |
| 4 | 兜底 | `Plain`（不着色，行为等同今天） |

调用点：

- `App::load_file_now`（`app.rs:285-300`）打开成功后 → `editor_state.set_language(detect(path))`；无扩展名时用 `lines.first()` 走 shebang。
- `App::write_buffer`（`app.rs:331-346`）另存改名后同样重设（`.txt` → `Plain`、`save as x.rs` → `Rust`）。

---

## 7. 主题与配色

> syntect 路线下，主题直接来自 `ThemeSet`（`.tmTheme`，与 VS Code / Sublime 生态通用），
> 本节的表退化为**降级映射**：`I_EDIT_COLOR=ansi16|256` 时把 RGB 主题量化到低位色深，
> `truecolor` 时直通。手写路线下，本节的表就是主题本身。

```rust
pub struct Theme {
    pub plain: Style, pub comment: Style, pub string: Style, pub number: Style,
    pub keyword: Style, pub type_: Style, pub function: Style,
    pub attribute: Style, pub operator: Style, pub punctuation: Style,
    pub property: Style, pub heading: Style, pub link: Style,
}

impl Theme {
    pub const ANSI16: Theme = Theme { /* ... */ };
    pub const MONO: Theme = Theme { /* 只留 comment dim + keyword bold */ };
    pub const fn style_for(&self, kind: TokenKind) -> Style;  // match，Copy 返回
}
```

`Style::new().fg(...)` 是 `const fn`，因此整个主题是编译期常量，`style_for` 无分支代价之外的开销、零分配。

**默认 `ANSI16` 配色**（16 色 ANSI，跟随终端主题；与现有 UI 的 DarkGray/White 基调一致）：

| Token | 颜色 | 修饰 |
| --- | --- | --- |
| comment | DarkGray | Italic |
| keyword | LightMagenta | — |
| type | LightCyan | — |
| function | LightBlue | — |
| string | LightGreen | — |
| number | LightYellow | — |
| attribute / macro（Rust `#[..]`、`foo!`） | LightRed | — |
| operator | Gray | — |
| punctuation | Gray | — |
| property（TOML/YAML 键、JSON 键） | LightBlue | — |
| heading（Markdown `#`） | LightCyan | Bold |
| link | LightBlue | Underlined |
| plain | 默认（Reset） | — |

**不做** `Color::Rgb` 默认主题：真彩在部分终端 / SSH 链路上会失真；`Theme` 是公开结构体，后续想加 `Theme::TRUECOLOR` 或读配置文件（需要 TOML 解析依赖）随时可加，属于独立决策。

---

## 8. 外壳集成

### 8.1 新动作与命令

- `src/action.rs:12-37`：`Action` 增 `ToggleSyntax`（unit 变体，不影响现有 `Clone` 成本）。
- `App::apply`（`app.rs:173-216`）：`Action::ToggleSyntax => { let on = self.editor_state.toggle_syntax(); self.message_box_state.info(...) }`。
- `src/widgets/command.rs:87-96` 的命令表加一行：
  ```
  "toggle syntax" => "turn syntax highlighting on or off" => Action::ToggleSyntax,
  ```
- **快捷键：v1 不加**。`Ctrl+Shift+* 组合在终端里上报不一致（例如 Ctrl+Shift+H 常被报成 Ctrl+H / 退格），`shortcut()`（`app.rs:524-549`）保持现状更安全；命令面板入口已足够。若后续要加，建议 `Ctrl+Shift+L` 并先实测终端上报。

### 8.2 状态栏显示语言

`StatusBar`（`status_bar.rs:95-164`）加一个 builder：

```rust
pub fn language(mut self, language: &'static str) -> Self   // 空串表示不显示
```

`left_line` 在 dirty 标记后追加一个 `Span::styled("  rust", style)`（借用 `&'static str`，零分配）。`App::render`（`app.rs:474-488`）从 `editor_state.language()` 取值传入。`Language::Plain` 返回空串，状态栏与今天一致。

### 8.3 消息

开关时给一条 `info` 消息（`message_box_state`，低频路径，允许一次 `String` 分配，属于 §3 接受项）：`syntax highlighting: on (rust)` / `off`。

---

## 9. 分配政策与验收

沿用 `tests/alloc_counting.rs` 的 `measure()` 计数分配器（`tests/alloc_counting.rs:93`）。

### 9.1 新增测点

| # | 测点 | 目标 | 备注 |
| --- | --- | --- | --- |
| S1 | 打开 .rs 1k 行后渲染一帧（高亮开） | **0** | 与现状 `2a` 同口径；runs 槽位跨帧复用 |
| S2 | 高亮开启后输入 100 个字符 | ≤ 1 | 与现状 `1a` 同口径（摊销增长） |
| S3 | 高亮开启后 ↓ ×100 | **0** | 与现状 `1b` 一致 |
| S4 | `set_language`（打开/另存时） | 1 | `states` Vec 一次，低频 |
| S5 | 冷启动首帧（含 1k 行 states 建立） | ≤ 3 | `states` + 少量 runs 容量增长 |
| S6 | 高亮关闭时渲染 1k 行 | **0**，且与 `2a` 数值相同 | G4 回归闸门 |

### 9.2 正确性测试（`src/syntax.rs` 模块内 + `viewport.rs` 渲染测试）

- 字符串内的 `//`、`#` 不开启注释；注释内的 `"` 不开启字符串；
- Rust 块注释跨行：`/* a` / `b */` 两行状态正确；`/* /* */ */` 嵌套深度正确；
- Markdown 围栏：`` ```rust `` 起、`` ``` `` 止，围栏内按 Rust 规则着色（v1 可先整块按 string 色，Phase 4 再递归）；
- 数字边界：`0x1F`、`1_000`、`1.5e-3` 不越过标识符；
- 宽字符与 run 边界：`let 中文 = "ab";` 的着色不越界、不丢字；
- `scroll_x > 0` 时 run 与字符对齐（byte 索引不受滚动影响）；
- 超长行触发 `MAX_RUNS_PER_LINE` 后文本仍完整（对比渲染出的 symbol 串与源行）；
- `enabled = false` 时对 1k 行渲染输出做 symbol 串快照对比（G4 硬验证）。

### 9.3 明确接受项

- `states: Vec<LineState>`：每文件一次（1 字节/行），与 `lines` 同级；
- `runs` 容量的摊销增长（稳态后不再增长）；
- 开关 / 切换语言时的消息 `String`（低频）；
- 第三方 `Buffer::set_style` 内部无分配（ratatui 单元格是 `Copy` 的 `Cell`）。

**不做**：不引入自定义分配器；不为高亮引入 `Box<dyn>`/trait object（语言分派用 `match`，静态派发）；不在渲染路径构造 `Span` / `Line` / `String`。

---

## 10. 分阶段实施

| Phase | 内容 | 涉及文件 | 预估行数 |
| --- | --- | --- | --- |
| 0 | `syntax.rs` 骨架：`Language::from_path`（扩展名 / 文件名 / shebang）、`Theme` 映射（syntect `Color` → ratatui `Color`，含 `I_EDIT_COLOR` 降级）+ 纯函数单测。不接 UI | `src/syntax.rs`（新）、`src/lib.rs` | ~150 |
| 1 | 语法集：`SyntaxSet` + `ThemeSet` 的 `OnceLock` 懒加载；把 syntect 的流式高亮按行切成 run，喂 `Highlights::replace_layer(LayerId::Base, ..)` + 单测 | `src/syntax.rs` | ~150 |
| 1' | *(备选 A)* 手写扫描器路线，替换 Phase 1：Rust 扫描器（块注释 / 字符串 / 数字 / 属性 / 宏 / 生命周期）+ `RunSink` | `src/syntax.rs` | ~250 |
| 2 | 渲染着色：`ViewportState` 持有 `Highlighter`，`viewport.rs` 文本循环加游标与 `set_style`（含宽字符第二格） | `src/widgets/viewport.rs` | ~40 |
| 3 | 失效接入：`EditorState::handle_key` / `load_file`；`App` 打开/另存时识别语言；状态栏显示 | `src/widgets/editor.rs`、`src/app.rs`、`src/widgets/status_bar.rs` | ~60 |
| 4 | 开关：新 `Action::ToggleSyntax`、命令面板一行、`App::apply` 分支、消息 | `src/action.rs`、`src/widgets/command.rs`、`src/app.rs` | ~30 |
| 5 | 其余语言：syntect 路线下只需往扩展名表里加行；手写路线下才是 6 门语言的扫描器 | `src/syntax.rs` | ~5 / ~300 |
| 6 | 测试固化：`alloc_counting.rs` 新增 S1–S6；关闭态渲染快照闸门 | `tests/alloc_counting.rs` | ~120 |

**提交切分**

1. `feat: language detection and theme mapping (no UI)`
2. `feat: syntect-backed base layer split into per-line runs`
3. `feat: detect language on open and save, show it in the status bar`
4. `feat: toggle syntax highlighting command`
5. `test: allocation and snapshot guards for highlighting`

> 注意：Phase 2/3/4/6 的"渲染着色、失效接入、开关"三项已被
> [`color-rendering-plan.md`](./color-rendering-plan.md) 的 Phase 1/2/3/5 覆盖（那是通用能力，
> 与本层无关）。本文实际只剩 Phase 0/1/5 + 语言识别 + 开关命令两处胶水。

Phase 5 的每一门语言可各自独立提交；Phase 6 的 S1–S6 建议随 3/4 一起落地，避免"先写红灯测试再补实现"的空窗。

---

## 11. 风险与回归

| 风险 | 影响 | 对策 |
| --- | --- | --- |
| 每帧重扫可见行的 CPU 成本 | 帧率 | 24×200 字符 ≈ 5k 次判断，<0.1 ms；S1 只验分配，另可加一个 `criterion` 之外的粗测（打印耗时，不建 bench 依赖） |
| `ensure_states` 最坏情况 O(行数) | 大文件首次下滚 | v1 接受；Phase 6 可选加 512 行检查点 |
| 病态长行 runs 爆炸 | 内存 / 帧时间 | `MAX_RUNS_PER_LINE = 512` 截断，回退 `Plain`，**不丢字**（有测试） |
| 宽字符第二格漏配样式 | 视觉断裂（中文注释/字符串） | 渲染循环同步 `set_style`；加"中文字符串贴右缘"测试 |
| `Plain` 写 default style 改变现有输出 | 回归 | `Style::default()` 与 `Buffer` 初始单元格等价；S6 + 快照测试兜底 |
| 主题在真彩终端/SSH 下失真 | 视觉 | 默认 ANSI16；`Rgb` 主题留接口不默认 |
| 状态缓存与文本不同步（漏失效） | 整屏错位 | 失效只走 `note_edit` 一个入口；`load_file` 强制 `reset`；加"编辑后跨行注释重算"测试 |
| 未来选区/搜索高亮无处安放 | 返工 | `TokenKind` 预留 `Selection` / `SearchMatch`（≥100）；叠加方式：先写 token 样式，再对命中区间二次 `set_style` 覆盖，仍零分配 |
| `ViewportState` 增加字段破坏既有测试 | 编译 | 现有测试用 `..Default::default()` 构造（`viewport.rs:142,158`），`Highlighter` 实现 `Default` 即可 |

**不做的事**：不做 LSP 与语义高亮（那是 `overlay` 层与 rust-analyzer 的事）；不做主题文件解析（v1）；不改动 `fs.rs` 的读写行为与行表示；不改动任何既有键位。

---

## 附录 A：首版词法规则

| 语言 | 行注释 | 块注释 | 字符串 | 数字 | 关键字来源 | 特殊 |
| --- | --- | --- | --- | --- | --- | --- |
| Rust | `//` | `/* */`（嵌套） | `"..."`、`'c'`、`r#"..."#`（v1 只处理裸 `r"..."` 与 `"..."`） | `0x` `0b` `0o`、下划线分隔、浮点、类型后缀 | 内置表（`fn let mut const struct enum impl trait pub use mod match if else loop while for return self Self` 等 + 原始类型 `i32 usize bool str` 着 Type 色） | `#[..]` 属性、`name!` 宏、生命周期 `'a` |
| Markdown | — | — | — | — | — | `#`~`######` 标题 Bold；`` ```lang `` 围栏内按 lang 递归（v1 整块 string 色）；`` ` `` 行内代码；`[text](url)` 链接 |
| TOML | `#` | — | `"..."` `'...'` | 整数/浮点/日期 | — | `[section]` 表头着 Type 色；`key =` 的 key 着 Property 色 |
| JSON | — | — | `"..."` | 数字 | `true` `false` `null` 着 Keyword 色 | 键（字符串后紧跟 `:`）着 Property 色 |
| YAML | `#` | — | `"..."` `'...'` | 数字 | `true` `false` `null` | `key:` 着 Property 色；`- ` 列表标记着 Punctuation |
| Shell | `#` | — | `"..."` `'...'` | 数字 | `if then fi for in do done case esac function export local return` | `$VAR` / `${VAR}` 着 Property 色 |
| Python | `#` | — | `"..."` `'...'`、`r/f/b` 前缀 | 数字 | `def class return if elif else for while import from as with try except finally lambda None True False` | 三引号字符串跨行（`Block::Str`） |

共用扫描骨架：`scan_line` 按 `Language` 分派到 7 个私有函数，共用"标识符 / 数字 / 字符串"三个原语，避免重复实现。

## 附录 B：改动点清单

| 文件 | 位置 | 改动 |
| --- | --- | --- |
| `src/syntax.rs` | 新增 | `TokenKind` / `Run` / `LineState` / `Language` / `Theme` / `Highlighter` / 各语言扫描器 + 单测 |
| `src/lib.rs` | `1-11` | 加 `pub mod syntax;` |
| `src/widgets/viewport.rs` | `22-32` | `ViewportState` 增 `highlight: Highlighter`（`pub(crate)`） |
| `src/widgets/viewport.rs` | `105-118` | 文本循环：run 游标 + `set_style`（含宽字符第二格） |
| `src/widgets/viewport.rs` | `123+` | 渲染测试：着色、宽字符、禁用态快照 |
| `src/widgets/editor.rs` | `242-292` | `handle_key` 记录 `y_before` / `lines_before`，编辑后 `note_edit` |
| `src/widgets/editor.rs` | `224-235` | `load_file` 末尾 `highlight.reset()` |
| `src/widgets/editor.rs` | `206-337` | 新增 `set_language` / `toggle_syntax` / `language()` 转发到 `viewport_state.highlight` |
| `src/app.rs` | `285-300` | `load_file_now` 成功后 `set_language(Language::from_path(..))`（含 shebang 兜底） |
| `src/app.rs` | `331-346` | `write_buffer` 后按新路径重设语言 |
| `src/app.rs` | `173-216` | `Action::ToggleSyntax` 分支 + 消息 |
| `src/app.rs` | `474-488` | `StatusBar` 构造传 `.language(..)` |
| `src/action.rs` | `12-37` | 新增 `ToggleSyntax` 变体 |
| `src/widgets/command.rs` | `87-96` | 命令表加 `toggle syntax` 一行 |
| `src/widgets/status_bar.rs` | `95-164` | `language` builder + `left_line` 追加一个 span |
| `src/fs.rs` | 不动 | 复用 `read_text_file` 的返回首行做 shebang 判断 |
| `tests/alloc_counting.rs` | 新增测点 | S1–S6（§9.1） |
