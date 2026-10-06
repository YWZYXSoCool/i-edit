# i-edit 选区 / 复制 / 粘贴 设计方案

> 状态：已落地（selection + copy/cut/paste + select-all + 系统剪贴板）｜ 日期：2026-10-06 ｜ 行号以当时版本为准，少数签名以最终实现为准（见内联标注）

**范围与口径**：为编辑器补全「选区 → 复制 → 粘贴」三段能力，并接入**系统级剪贴板**（与操作系统剪贴板互通，可跨应用复制 / 粘贴）。
- 选区：基于 Shift 的方向键扩展，支持字符级与行级两种模式；
- 复制 / 剪切：把选区（或无选区时的当前行）写入系统剪贴板，**同时**镜像到内部寄存器（作为后端不可用时的兜底）；
- 粘贴（Ctrl+V）：优先读取系统剪贴板，取不到（后端不可用 / 远程终端不支持读回）时回退到内部寄存器；
- 粘贴（外部）：终端括号粘贴 `Event::Paste(text)` 直接插入光标处，不污染内部寄存器；
- 系统剪贴板后端：本地优先用原生 API（Windows 下即 Win32 剪贴板，经 `arboard` 封装），远程 / 无显示环境回退到 OSC 52 转义序列；两者都不可用时退化为纯内部寄存器。

**与既有文档的关系**：本方案复用 [`color-rendering-plan.md`](./color-rendering-plan.md) 的 `Highlights::overlay` 层来画选区背景——`syntax-highlight-plan.md` 已明确「`base` = 语言着色，`overlay` = 选区 / 搜索高亮」，两者互不冲突。零分配政策沿用 [`allocation-plan.md`](./allocation-plan.md)：选区叠加层只在选区 / 光标 / 文本变化时重算，稳态渲染零分配。

---

## 目录

- [1. 背景与现状](#1-背景与现状)
- [2. 目标与验收](#2-目标与验收)
- [3. 设计原则](#3-设计原则)
- [4. 数据模型](#4-数据模型)
- [5. 选区算法](#5-选区算法)
- [6. 键位设计](#6-键位设计)
- [7. 选区渲染](#7-选区渲染)
- [8. 剪贴板](#8-剪贴板)
- [9. 复制 / 剪切 / 粘贴 算法](#9-复制--剪切--粘贴-算法)
- [10. 接入点](#10-接入点)
- [11. 分阶段实施](#11-分阶段实施)
- [12. 风险与回归](#12-风险与回归)
- [13. 测试方案](#13-测试方案)
- [附录 A：改动点清单](#附录-a改动点清单)

---

## 1. 背景与现状

- `TextState`（`src/text.rs`）只有单光标（`cursor: Cursor { x: usize, y: usize }`，`x` 为当前行的**字节偏移**，永远落在字符边界上），**没有任何选区概念**。
- `EditorState::handle_key`（`src/widgets/editor.rs:263`）是一个大 `match`，移动键不带 Shift 语义；`modifiers` 只用到 `CONTROL` 与 `NONE`，`SHIFT` 尚未参与。
- `Action` 枚举（`src/action.rs`）与 `SHORTCUTS`（`src/shortcuts.rs`）均**无**复制 / 剪切 / 粘贴 / 选区相关动作。
- 渲染侧 `ViewportState::highlights: Highlights`（`src/highlight.rs`）已有 `base` / `overlay` 两层 `StyledRun` 叠加机制，且 `overlay` 当前**空闲**（语法高亮计划里它被预留给选区 / 搜索）。`Viewport::render` 已支持 overlay 覆盖 base、宽字符第二格同色、行尾背景填充。
- 文件树（`src/widgets/file_tree.rs`）有自己的 `selected` 行高亮，与编辑区文本选区**无关**，本方案不改动它。
- 主循环 `App::run`（`src/app.rs:152-156`）在 `utils::key_press(&event)` 返回 `None` 时直接 `continue`——意味着 `Event::Paste` 这类非按键事件**当前根本到不了任何组件**，是我們接外部粘贴必须改的一处。

结论：选区 / 复制 / 粘贴对编辑器是**全新能力**，但可完全架在已有的 `Highlights::overlay` 之上，无需新建渲染通道。

---

## 2. 目标与验收

| 目标 | 说明 |
| --- | --- |
| G1 字符级选区 | Shift+方向键 / Home / End / PageUp/Down / Ctrl+方向键（词）扩展光标选区 |
| G2 行级选区 | Shift 配合「移动到行首/行尾/文首/文尾」可形成整行选区（复制即整行带换行） |
| G3 复制 | Ctrl+C 把选区写入内部寄存器；无选区时复制当前行 |
| G4 剪切 | Ctrl+X 复制并删除选区；无选区时剪切当前行 |
| G5 粘贴（内部） | Ctrl+V / Shift+Insert 把寄存器插入光标处，多行文本正确成行 |
| G6 粘贴（外部） | 终端括号粘贴 `Event::Paste(text)` 直接插入光标处 |
| G7 选区可视化 | 选区以反显（reverse）背景渲染，跟随滚动且宽字符不破块 |
| G8 每帧零分配 | 稳态渲染一帧、稳态移动光标，本项目触发的堆分配 = 0（与现状一致） |
| G9 关闭时逐字节等价 | 无选区时渲染输出与今天完全相同（不写 overlay，或 overlay 为空） |
| G10 系统剪贴板互通 | 复制 / 剪切写入系统剪贴板（其他应用可粘贴）；Ctrl+V 优先读取系统剪贴板，失败时回退内部寄存器 |
| G11 全选 | Ctrl+A 形成覆盖全部行的行级选区，便于一键复制 / 删除整篇 |

**不做**（v1）：多选（multiple cursors）、列块（矩形）选区、选区与搜索高亮叠加、撤销/重做对选区的特殊处理。（系统剪贴板已纳入 v1，见 §8。）

---

## 3. 设计原则

1. **选区是编辑器视图状态，不是文本状态**。选区属于 `EditorState`，`TextState` 只提供「给定选区，返回/删除/插入文本」的纯几何操作，保持与 `Input` 组件共享时不受影响。
2. **复用 `overlay` 层画选区**，不新建渲染通道，与 `syntax-highlight-plan.md` 的层分工一致（`base`=语言，`overlay`=选区）。
3. **Shift 是方向键的「扩展」修饰符**。方向键不带 Shift = 移动并收起选区；带 Shift = 扩展选区。这与终端可靠上报 Shift+方向键的事实吻合（`shortcuts.rs` 注释已说明 Ctrl+Shift 不可靠，但纯 Shift 没问题）。
4. **零分配走「变化时重算 + 容量复用」**，与语法高亮的 `replace_layer` 同策略：选区 overlay 只在选区 / 光标 / 行数变化时重算，之后 `replace_layer` 复用 arena 与 `lines` 容量，稳态零分配。
5. **系统剪贴板为粘贴的首选来源，内部寄存器为兜底**。复制 / 剪切同时写系统剪贴板与内部寄存器；Ctrl+V 优先读系统剪贴板，取不到（后端不可用 / OSC 52 读回不受支持）时回退内部寄存器。外部粘贴（`Event::Paste`）直接插入，不碰任一来源。

---

## 4. 数据模型

### 4.1 选区类型

新类型放在 `src/text.rs`（它是文本几何，与 `Input` 共享），`EditorState` 持有其实例。

```rust
/// 选区的两种粒度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectionMode {
    /// 没有选区。
    #[default]
    None,
    /// 字符级：锚点到光标的精确字节区间。
    Char,
    /// 行级：锚点行到光标行之间的整行（含行尾换行）。
    Line,
}

/// 一段选区：固定锚点 + 活动端（活动端就是 `TextState.cursor`）。
///
/// 不存终点副本——终点随光标移动，省一个字段也避免两端不同步。
/// 锚点 + 活动端谁在前由算法按 `(y, x)` 排序决定，不在此固化方向。
#[derive(Debug, Clone, Copy, Default)]
pub struct Selection {
    pub anchor: Cursor,
    pub mode: SelectionMode,
}

impl Selection {
    /// 是否处于有效选区（非 None）。
    pub fn is_active(&self) -> bool { self.mode != SelectionMode::None }

    /// 同时考虑锚点与当前光标，给出有序的 (start_y, start_x, end_y, end_x)。
    pub fn ordered(&self, cursor: Cursor) -> (usize, usize, usize, usize);
}
```

### 4.2 EditorState 新增字段

```rust
pub struct EditorState {
    pub text: TextState,
    // ... 现有字段 ...
    /// 当前选区；`mode == None` 表示无选区。
    selection: Selection,
    /// 内部剪贴板寄存器，跨文件存活于本次编辑会话。
    clipboard: Clipboard,
    /// 选区 overlay 是否需要重算；任何选区/光标/行数变化都置 true。
    selection_dirty: bool,
    /// 选区 overlay 行缓冲，重算时复用容量（稳态零分配）。
    selection_rows: Vec<Vec<StyledRun>>,
}
```

---

## 5. 选区算法

所有几何操作落在 `TextState`，接受 `&Selection` 与当前 `cursor`。

### 5.1 起点与扩展

- `selection_begin(mode)`：把 `anchor` 设为当前 `cursor`，`mode` 设为给定值。在第一次 Shift+移动时调用。
- 扩展即「移动光标 + 保留锚点」：Shift+方向键直接调用对应的 `move_*`（`move_left/right/up/down/word_left/word_right/to_line_start/...`），**不**调用 `selection_clear`。锚点不动，活动端（光标）随移动推进。
- 行级选区的锚点/活动端在排序时按「整行」处理：见 §5.3。

### 5.2 收起

- 普通移动键（不带 Shift）命中时，先 `selection_clear()` 再移动——与主流编辑器一致：无 Shift 移动即折叠选区。
- `selection_clear()`：把 `mode` 置 `None`，并置 `selection_dirty`（渲染层据此清空 overlay）。

### 5.3 `ordered()`：有序字节区间

返回 `(sy, sx, ey, ex)`，满足 `(sy, sx) ≤ (ey, ex)`（按行优先、行内按字节）。供复制 / 删除 / 渲染共用，避免各算一遍。

- **Char 模式**：直接比较 `(anchor.y, anchor.x)` 与 `(cursor.y, cursor.x)` 排序。
- **Line 模式**：取 `min(y)` 与 `max(y)` 作为首末行；首行从前半段任意端点到行尾（`sx = 0, ex = line_len`），末行从行首到后半段任意端点（`sx = 0`）；中间行整行。这样不论从哪端开始拖，选中的都是完整行。

### 5.4 跨行提取 / 删除 / 插入

见 §9，统一用 §5.3 的有序区间。

---

## 6. 键位设计

选区扩展走 `EditorState::handle_key` 内部分支（与现有移动键一样，不经过 `Action` / `SHORTCUTS` 表）。复制 / 剪切 / 粘贴也是编辑器内部动作，同样在 `handle_key` 里拦截。

| 组合 | 行为 | 备注 |
| --- | --- | --- |
| Shift+← / → | 字符级扩展 1 字符 | 首次触发 `selection_begin(Char)` |
| Shift+↑ / ↓ | 字符级扩展 1 行 | 同上 |
| Shift+Ctrl+← / → | 词级扩展 | 复用 `move_word_left/right` |
| Shift+Home / End | 扩展到行首 / 行尾 | `move_to_line_start/end` |
| Shift+Ctrl+Home / End | 扩展到文首 / 文尾 | `move_to_text_start/end` |
| Shift+PageUp / PageDown | 扩展到上一屏 / 下一屏 | `move_page_up/down(PAGE_SIZE)` |
| 普通 ←/→/↑/↓/Home/... | 移动并收起选区 | 见 §5.2 |
| Ctrl+C | 复制（选区或当前行） | 无选区 → 复制当前行 |
| Ctrl+X | 剪切（选区或当前行） | 无选区 → 剪切当前行 |
| Ctrl+V / Shift+Insert | 粘贴（系统优先，回退内部寄存器） | 见 §8 |
| Ctrl+A | 全选（行级选区覆盖所有行） | `select_all()` |
| Esc | 收起选区，光标不动 | `selection_clear()` |
| 输入字符 / Enter / Backspace / Delete | 选区存在时先删选区再执行 | 等价「替换选区」 |
| （终端）`Event::Paste(text)` | 插入 `text` 到光标 | 见 §10 |

**终端可靠性**：纯 Shift + 方向键 / Home / End 上报稳定，可放心绑定。Ctrl+C / Ctrl+X / Ctrl+V 在 raw mode 下作为 `KeyEvent` 上报（不会触发 SIGINT），当前 `handle_key` 的 `(_, KeyCode::Char(c))` 兜底分支会把它当成插入字符——**必须在兜底插入之前**显式拦截这三个组合。

---

## 7. 选区渲染

### 7.1 复用 overlay 层

在 `Editor::render` 构造 `Viewport` **之前**，若 `state.selection.is_active()` 且 `state.selection_dirty`：

```rust
state.recompute_selection_overlay();   // 见下
state.selection_dirty = false;
state.viewport_state.highlights.set_enabled(true);
```

`recompute_selection_overlay` 只算**可见行** `[scroll_y .. scroll_y + height)`，产出 `Vec<Vec<StyledRun>>` 后 `replace_layer(LayerId::Overlay, &rows)`。

选区样式用反显，任何终端主题下都可读、不会与语言着色的前景色冲突：

```rust
const SELECTION_STYLE: Style = Style::new().add_modifier(Modifier::REVERSED);
```

- **Char 模式**：每行一段 `[start, end)` 的 `StyledRun{ style: SELECTION_STYLE }`（`start/end` 为字节偏移，与 `Viewport` 的 byte-cursor 对齐，`scroll_x` 不影响着色）。
- **Line 模式**：每行 `[0, line_len)` 的 `StyledRun`。`viewport.rs` 的 `trailing_background` 会把到达行尾且带 `bg` 的 run 填充到可见行尾——反显修饰同样让整行（含行尾空白）反显，读起来是一条选择条。
- 宽字符第二格：`Viewport::render` 已经把宽字符第二格写成同 `style`，选区反显天然覆盖，不破块。

### 7.2 收起 / 关闭

- 选区收起（`selection_clear`）时 `selection_dirty = true`；下一次 render 若 `!is_active()`，调用 `clear_layer(LayerId::Overlay)` 并（若没有其他 overlay 生产者）`set_enabled(false)`，渲染输出与今天逐字节一致（G9）。
- v1 没有 `base` 生产者，所以 `set_enabled(false)` 即回到无着色态；未来语法高亮上 `base` 后，enabled 应由 base 生产者控制，本方案的 overlay 清空不影响 base。

### 7.3 零分配口径

`replace_layer` 内部 `arena.clear()` + `extend`（容量够则零分配），`lines` 向量 `clear()` + `reserve(rows.len())` + `push`（稳态行数固定，reserve 不重分配）。即：初次显示选区会建容量，之后每次光标移动重算**复用**容量 → 稳态零分配。与 `alloc_counting` 口径一致，新增测点见 §13。

### 7.4 与 `PaintDemo` 的冲突

`Highlights::paint_demo` 也写 `overlay`（临时演示）。两路写同一个 overlay 会互相覆盖。选区是常驻功能，`PaintDemo` 注释已标明「temporary, removable」——在本方案落地阶段一并**移除 `paint_demo` 调用与 `Action::PaintDemo`**，避免冲突，也契合 `color-rendering-plan.md` 所述「demo 是临时占位」。

---

## 8. 剪贴板（含系统级）

新模块 `src/clipboard.rs`（`lib.rs` 加 `pub mod clipboard;`）。目标是让 **复制 / 剪切** 真正进入操作系统剪贴板（其他应用可粘贴），且 **Ctrl+V** 能粘贴从其他应用复制来的内容。

### 8.1 抽象

```rust
/// 后端选择，可由配置覆盖，默认 Auto。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClipboardBackendKind {
    /// 本地有显示环境用原生，否则 OSC 52，再否则纯内部。
    #[default] Auto,
    /// 操作系统原生剪贴板（Windows = Win32）。
    Native,
    /// OSC 52 转义（SSH / tmux 等远程终端）。
    Osc52,
    /// 仅内部寄存器，不碰系统。
    InternalOnly,
}

/// 系统剪贴板后端：读 / 写。
pub trait ClipboardBackend: Send {
    /// 读取系统剪贴板文本；不支持读或当前不可用时返回 None（调用方回退内部寄存器）。
    fn read(&self) -> Option<String>;
    /// 写入系统剪贴板；成功返回 true。需 `&mut self`：原生后端要锁可变引用，
    /// OSC 52 后端要把转义序列写进自身持有的 writer。
    fn write(&mut self, text: &str) -> bool;
}

/// 原生后端：封装 arboard::Clipboard（惰性初始化，Mutex 包裹以满足 Send/Sync）。
pub struct NativeBackend { inner: Mutex<arboard::Clipboard> }

/// OSC 52 后端：write 时 base64 编码后输出 `\x1b]52;c;<b64>\x07`；read 返回 None。
pub struct Osc52Backend<W: Write> { writer: W }
```

`Clipboard` 管理器持有「内部兜底寄存器 + 系统后端」：

```rust
pub struct Clipboard {
    register: String,                    // 内部兜底寄存器
    backend: Box<dyn ClipboardBackend>,  // 系统后端（可能退化为内部）
}

impl Clipboard {
    /// 复制：内部寄存器与系统剪贴板双写；任一失败不影响另一路。
    pub fn copy(&mut self, text: &str) {
        self.register.clear();
        self.register.push_str(text);
        let _ = self.backend.write(text);
    }
    /// 剪切：语义同 copy（删文本由调用方负责）。
    pub fn cut(&mut self, text: &str) { self.copy(text); }
    /// 粘贴：优先系统剪贴板，取不到 / 空则回退内部寄存器。
    pub fn paste_text(&self) -> String {
        self.backend.read()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| self.register.clone())
    }
}
```

### 8.2 后端选择策略

`Auto`（默认）按运行环境挑一个可用后端：

| 环境 | 选用后端 | 说明 |
| --- | --- | --- |
| 本地桌面（Windows / macOS / Linux 有显示） | `NativeBackend`（arboard） | 直接读写 OS 剪贴板，双向互通 |
| 远程 / 终端（CI、SSH、tmux，且 arboard 初始化失败） | `Osc52Backend` | 写入走转义可进系统剪贴板；读取终端一般不支持 → 回退内部 |
| 编译时关掉 `system-clipboard` feature 或均不可用 | 退化内部寄存器 | 仍可在应用内复制粘贴，仅不跨应用 |

用户可在配置里显式指定 `Native / Osc52 / InternalOnly / Auto`（若项目已有 settings 模块则加字段 `clipboard: ClipboardBackendKind`，否则用 `const DEFAULT_BACKEND: ClipboardBackendKind = Auto`）。

### 8.3 依赖

Cargo.toml 新增（原生走 feature，默认开）：

```toml
[dependencies]
base64 = "0.22"
arboard = { version = "3", optional = true }

[features]
default = ["system-clipboard"]
system-clipboard = ["dep:arboard"]
```

- `arboard`：跨平台原生剪贴板（Windows 下即 Win32 `OpenClipboard` / `SetClipboardData`），读写都原生支持。
- `base64`：OSC 52 写入时需要把文本 base64 编码。
- 内部寄存器始终可用，不依赖任何外部库；关掉 feature 时整体退化为内部，编译不受影响。

### 8.4 与既有设计的差异

- 旧方案「内部寄存器为唯一权威来源、系统剪贴板列为后续阶段」作废；现在**系统剪贴板是首选来源，内部寄存器是兜底**（见 §3 原则 5）。
- `Event::Paste`（终端括号粘贴）仍直接插入、**不写**任一来源（也不污染寄存器）——它本身就是终端把系统剪贴板内容投递进来的通道，无需再绕。
- 复制 / 剪切因为是用户主动动作（不在渲染热路径），允许内部 `String` 与系统后端各分配一次，零分配政策的 SL3 / SL4 对「走系统后端」的情况放宽（见 §13.3）。
- `Osc52Backend` 的 writer 在构造时注入（生产环境传终端的 stdout，测试环境传 `Vec<u8>` 或 `io::sink`），便于单测而不触碰真实终端。

---

## 9. 复制 / 剪切 / 粘贴 算法

以下三个操作都基于 §5.3 的 `ordered()`。`TextState` 提供纯几何方法，`EditorState` 负责脏标记 / 版本 / 选区收起。

### 9.1 提取 `selected_text`

```rust
impl TextState {
    /// 按选区返回应复制的文本；无选区时调用方改传「当前行 + 换行」。
    pub fn selected_text(&self, sel: &Selection) -> String {
        let (sy, sx, ey, ex) = sel.ordered(self.cursor);
        let mut out = String::new();
        for y in sy..=ey {
            let line = &self.lines[y];
            let (a, b) = if y == sy && y == ey {
                (sx, ex)
            } else if y == sy {
                (sx, line.len())
            } else if y == ey {
                (0, ex)
            } else {
                (0, line.len())
            };
            out.push_str(&line[a..b]);
            if sel.mode == SelectionMode::Line || y != ey {
                out.push('\n');   // 行级必带；字符级跨行时行间也要分隔
            }
        }
        out
    }
}
```

- Char 模式：行间补 `\n`；纯单行不补尾换行（除非恰好整行）。
- Line 模式：每行带 `\n`。

### 9.2 删除 `delete_selection`

```rust
impl TextState {
    /// 删除选区并返回（不写寄存器）。光标落在删除区起点。
    pub fn delete_selection(&mut self, sel: &Selection) {
        let (sy, sx, ey, ex) = sel.ordered(self.cursor);
        if sy == ey {
            self.lines[sy].drain(sx..ex);
        } else {
            let head = self.lines[sy][..sx].to_owned();
            let tail = self.lines[ey][ex..].to_owned();
            self.lines[sy] = head + &tail;
            self.lines.drain((sy + 1)..=ey);
        }
        self.cursor = Cursor { x: sx, y: sy };
        self.clamp_cursor();
    }
}
```

### 9.3 插入 `insert_str`（多行）

```rust
impl TextState {
    /// 在光标处插入字符串，内部 '\n' 正确切分成多行。
    pub fn insert_str(&mut self, text: &str) {
        let (x, y) = (self.cursor.x, self.cursor.y);
        let parts: Vec<&str> = text.split('\n').collect();
        if parts.len() == 1 {
            self.lines[y].insert_str(x, parts[0]);
            self.cursor.x = x + parts[0].len();
            return;
        }
        let before = self.lines[y][..x].to_owned();
        let after  = self.lines[y][x..].to_owned();
        let mut new_lines = Vec::with_capacity(parts.len());
        new_lines.push(before + parts[0]);
        for mid in &parts[1..parts.len() - 1] {
            new_lines.push(mid.to_string());
        }
        let last = parts[parts.len() - 1].to_string() + &after;
        new_lines.push(last);
        // 用 new_lines 替换 lines[y]，并把中段插入其后
        self.lines[y] = new_lines.remove(0);
        let insert_at = y + 1;
        for (i, l) in new_lines.into_iter().enumerate() {
            self.lines.insert(insert_at + i, l);
        }
        let last_len = parts[parts.len() - 1].len();
        self.cursor = Cursor { x: last_len, y: y + parts.len() - 1 };
        self.clamp_cursor();
    }
}
```

### 9.4 编排（在 `EditorState`）

```rust
impl EditorState {
    fn copy(&mut self) {
        let text = if self.selection.is_active() {
            self.text.selected_text(&self.selection)
        } else {
            // 无选区：复制当前整行（VS Code 行为）
            let line = &self.text.lines[self.text.cursor.y];
            let mut s = line.clone(); s.push('\n'); s
        };
        self.clipboard.copy(&text);
        // 复制不改 buffer：不动 dirty / version，选区保留（便于多次粘贴）
    }

    fn cut(&mut self) {
        let text = if self.selection.is_active() {
            self.text.selected_text(&self.selection)
        } else {
            let line = &self.text.lines[self.text.cursor.y];
            let mut s = line.clone(); s.push('\n'); s
        };
        self.clipboard.copy(&text);
        if self.selection.is_active() {
            self.text.delete_selection(&self.selection);
        } else {
            // 无选区剪切当前行：删除整行并上提
            self.text.lines.remove(self.text.cursor.y);
            if self.text.lines.is_empty() { self.text.lines.push(String::new()); }
            self.text.cursor = Cursor { x: 0, y: self.text.cursor.y.min(self.text.lines.len()-1) };
            self.text.clamp_cursor();
        }
        self.after_edit(self.text.cursor.y);   // 见 §10
        self.selection_clear();
    }

    fn paste(&mut self) {
        let text = self.clipboard.paste_text();   // 系统剪贴板优先，回退内部寄存器
        // 选区仍激活时先替换选区（标准编辑器行为）。
        if self.selection.is_active() {
            self.text.delete_selection(&self.selection);
            self.selection_clear();
        }
        self.text.insert_str(&text);
        self.after_edit(self.text.cursor.y);
        self.selection_clear();
    }

    /// 外部粘贴（终端括号粘贴）：同样替换活动选区，且不污染内部寄存器。
    fn paste_text(&mut self, text: &str) {
        if self.selection.is_active() {
            self.text.delete_selection(&self.selection);
            self.selection_clear();
        }
        self.text.insert_str(text);
        self.after_edit(self.text.cursor.y);
        self.selection_clear();
    }

    /// 全选：行级选区覆盖从首行到末行的全部内容，光标落在末行行尾。
    fn select_all(&mut self) {
        let last = self.text.lines.len().saturating_sub(1);
        self.selection.anchor = Cursor { x: 0, y: 0 };
        self.text.cursor = Cursor { x: self.text.lines[last].len(), y: last };
        self.selection.mode = SelectionMode::Line;
        self.after_move();
    }
}
```

`after_edit(y)` 复用现有 `handle_key` 结尾的脏标记逻辑：`dirty = true; version += 1; highlights.note_edit(y); selection_dirty = true; sync()`（见 §10）。

---

## 10. 接入点

### 10.1 `handle_key` 重构（字符级 Shift 感知）

把「移动」抽成一个内部 `Motion` 枚举，`SHIFT` 决定 move 还是 extend：

```rust
fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
    use KeyModifiers as M;
    let shift = modifiers.contains(M::SHIFT);
    let ctrl  = modifiers.contains(M::CONTROL);

    // 1) 拦截复制 / 剪切 / 粘贴（须在字符插入兜底之前）
    if ctrl {
        match code {
            KeyCode::Char('c') => { self.copy();  return; }
            KeyCode::Char('x') => { self.cut();   return; }
            KeyCode::Char('v') => { self.paste(); return; }
            _ => {}
        }
    }
    if !ctrl && modifiers.contains(M::SHIFT) && code == KeyCode::Insert {
        self.paste(); return;
    }

    // 2) 移动 / 扩展
    if let Some(m) = Motion::from(modifiers, code) {
        if shift {
            if !self.selection.is_active() { self.selection_begin(SelectionMode::Char); }
            m.extend(&mut self.text);          // 复用现有 move_*，锚点不动
        } else {
            if self.selection.is_active() { self.selection_clear(); }
            m.apply(&mut self.text);
        }
        self.after_move();   // 仅重算 overlay + 同步视图，不置 dirty（移动不改文档）
        return;
    }

    // 3) 编辑：选区存在则先删选区（替换语义）
    let had_sel = self.selection.is_active();
    if had_sel {
        self.text.delete_selection(&self.selection);
        self.selection_clear();
    }

    let mut edited = false;
    match (modifiers, code) {
        (_, KeyCode::Char(c))     => { self.text.insert_char(c);  edited = true; }
        (M::NONE, KeyCode::Enter) => { self.text.insert_new_line(); edited = true; }
        (M::NONE, KeyCode::Backspace) => { self.text.delete_backward(); edited = true; }
        (M::NONE, KeyCode::Delete)    => { self.text.delete_forward();  edited = true; }
        (M::NONE, KeyCode::Esc)  => { self.selection_clear(); }
        _ => {}
    }
    if edited || had_sel { self.after_edit(self.text.cursor.y); }
    else { self.sync(); }
}
```

`Motion::from` 把 `(modifiers, code)` 映射到 `Left/Right/Up/Down/LineStart/LineEnd/WordLeft/WordRight/TextStart/TextEnd/PageUp/PageDown`，并区分是否 `ctrl`（词 / 文首文尾 / 翻页）。`extend` 与 `apply` 都调用 `TextState` 现有的 `move_*`，区别仅在于 `extend` 不动 `anchor`。

### 10.2 `Editor::handle_event` 接收粘贴事件

当前 `Editor::handle_event` 先 `key_press` 返回 `None` 就 `return`，收不到 `Event::Paste`。改为：

```rust
fn handle_event(self, event: &Event, state: &mut Self::State) {
    match event {
        Event::Paste(text) => state.paste_text(text),
        _ => {
            let Some(key) = crate::utils::key_press(event) else { return; };
            state.handle_key(key.code, key.modifiers);
        }
    }
}
```

### 10.3 `App::run` 转发 `Event::Paste`

主循环当前 `let Some(key) = utils::key_press(&event) else { continue; }` 会把 `Event::Paste` 直接丢棄。改为：

```rust
let event = event::read()?;
match &event {
    Event::Paste(_) if self.focus == Focus::Editor && self.popup_state.kind.is_none() => {
        Component::handle_event(Editor, &event, &mut self.editor_state);
    }
    _ => {
        let Some(key) = utils::key_press(&event) else { continue; };
        // ... 现有 StatusBar / shortcut / focus / Editor 派发 ...
    }
}
```

（`Event::Resize` 仍按现状走 `continue`，交给 `terminal.draw` 处理尺寸。）

### 10.4 `load_file` 与焦点切换

- `EditorState::load_file`（editor.rs:243）末尾加 `self.selection_clear();`，并置 `selection_dirty` → 新文件不残留旧选区与旧 overlay。
- `set_focus` 切到文件树再切回时选区保留（VS Code 行为），无需特殊处理。

---

## 11. 分阶段实施

| Phase | 内容 | 涉及文件 | 预估行数 |
| --- | --- | --- | --- |
| 0 | `Selection` / `SelectionMode` 类型 + `TextState::selected_text` / `delete_selection` / `insert_str` + 单元测 | `src/text.rs`、`src/lib.rs` | ~120 |
| 1 | `Clipboard` 模块：后端抽象 `ClipboardBackend` + `NativeBackend`（arboard，feature 门控）/ `Osc52Backend`（base64）/ 退化内部；`Cargo.toml` 加 `base64`、`arboard`(optional) 与 `system-clipboard` feature；`Auto` 后端选择；`Clipboard::copy/cut/paste_text` | `src/clipboard.rs`（新）、`src/lib.rs`、`Cargo.toml` | ~140 |
| 2 | `EditorState` 字段 + `handle_key` 重构（Shift 扩展、Ctrl+C/X/V、Esc、替换语义、`after_edit`） | `src/widgets/editor.rs` | ~120 |
| 3 | 选区渲染：`recompute_selection_overlay` + `Editor::render` 接入 overlay；移除 `PaintDemo` | `src/widgets/editor.rs`、`src/widgets/viewport.rs`（不动）、`src/action.rs`（删 `PaintDemo`）、`src/app.rs`（删调用） | ~60 |
| 4 | 外部粘贴：`Editor::handle_event` 收 `Event::Paste` + `App::run` 转发 | `src/widgets/editor.rs`、`src/app.rs` | ~25 |
| 5 | 测试固化：`alloc_counting` 新增选区测点；编辑器键位 / 渲染快照测试 | `tests/alloc_counting.rs`、`src/widgets/editor.rs` | ~150 |

**提交切分**

1. `feat: text selection geometry (extract/delete/insert)`
2. `feat: system clipboard backends (native via arboard, osc52 fallback, internal fallback) + Cargo deps/feature`
3. `feat: shift-selection and copy/cut/paste keys`
4. `feat: render selection on the overlay layer (drop PaintDemo)`
5. `feat: support terminal bracketed paste`
6. `test: allocation and key/render guards for selection + clipboard backend`

---

## 12. 风险与回归

| 风险 | 影响 | 对策 |
| --- | --- | --- |
| `overlay` 与 `PaintDemo` 冲突 | 选区被 demo 覆盖或反之 | 同阶段移除 `PaintDemo`（§7.4） |
| 选区 overlay 每帧重算分配 | 违反零分配政策 | 只算可见行 + `selection_dirty` 门控 + `replace_layer` 容量复用（§7.3） |
| `scroll_x > 0` 时选区错位 | 着色偏移字符 | overlay 用字节区间，`Viewport` byte-cursor 已处理，与语法高亮同口径 |
| 宽字符第二格漏反显 | 选区破块 | 复用 `Viewport` 既有宽字符第二格同 `style` 逻辑（§7.1） |
| 无选区时 Ctrl+C / Ctrl+X 删错内容 | 误删整行 | 明确「无选区 = 当前行」语义并单测（§9.4） |
| 行级选区跨行提取少 / 多换行 | 复制内容不准 | `ordered()` 统一行级整行 + 每行 `\n`，单测边界 |
| `insert_str` 非 UTF-8 边界 | panic | 插入串本身合法 UTF-8；`split('\n')` 不破坏字符；`clamp_cursor` 兜底 |
| Ctrl+C 在 raw mode 下被当 SIGINT | 复制失效 | ratatui/crossterm raw mode 已把 Ctrl+C 作为 `KeyEvent` 上报；需在兜底插入前拦截（§6） |
| `Event::Paste` 到不了编辑器 | 外部粘贴失效 | `App::run` 转发 + `Editor::handle_event` 匹配（§10.3） |
| 切换文件残留选区 / overlay | 视觉错乱 | `load_file` 调 `selection_clear()`（§10.4） |
| 选区与语法高亮未来抢 `overlay` | 返工 | 本方案已遵循 `base=语言 / overlay=选区` 分工，不冲突 |
| 寄存器跨文件存活 | 设计预期 | `Clipboard` 挂在 `EditorState`，单次会话单缓冲，符合预期 |
| 原生后端初始化失败（无显示 / 沙箱 / CI） | 系统剪贴板不可用 | `Auto` 捕获 arboard 初始化 `Err` → 降级 `Osc52Backend` → 再降级内部；不 panic |
| OSC 52 读回不可靠 | Ctrl+V 读不到系统内容 | `Osc52Backend::read` 一律 `None`，`paste_text` 回退内部寄存器；明确「写入成功 ≠ 能读回」 |
| `arboard::Clipboard` 非 `Sync` | `EditorState` 若被 `Arc` 共享会编译失败 | 用 `Mutex<arboard::Clipboard>` 包裹，满足 `Send + Sync` |
| 复制 / 剪切的额外堆分配 | 违反零分配政策 | 属用户主动动作，不在渲染热路径；放开 SL3（见 §13.3） |
| 双写不一致 / 丢数据 | 复制后粘不出 | 先写内部再写系统，任一失败不影响另一路；`paste_text` 系统优先、回退内部，不丢 |
| Windows 剪贴板历史（Win+V）兼容 | 复制的内容进不了历史 | arboard 写入标准 `CF_UNICODETEXT`，与系统历史兼容 |
| 受限环境权限（Linux Wayland / 沙箱） | 原生后端失败 | 已降级到 OSC52 / 内部，不阻断编辑 |

---

## 13. 测试方案

### 13.1 单元（无渲染）

`src/text.rs`：
- `selected_text` 字符级单行 / 跨行 / 反向（锚点在后）正确拼接与换行；
- `selected_text` 行级整行带 `\n`；
- `delete_selection` 单行 / 跨行后光标落在起点，行数正确；
- `insert_str` 单行无换行、多行切分、`cursor` 落在末行末字节、宽字符不破界；
- `Selection::ordered` 锚点在后时排序正确。

`src/widgets/editor.rs`：
- Shift+→ 后 `selection.is_active()` 且锚点 = 起点；继续 Shift+← 收起逻辑（无 Shift 移动收起）；
- Ctrl+C 后寄存器内容 = 选区文本；Ctrl+X 后文本被删且寄存器有值；
- Ctrl+V 后光标处插入寄存器，多行成多行；
- 无选区 Ctrl+C 复制当前行（带 `\n`）；
- `Event::Paste("a\nb")` 插入两行、`cursor` 在 `b` 末；
- `load_file` 后 `selection` 为 None、overlay 清空。

### 13.1b 剪贴板模块（无渲染）

`src/clipboard.rs`：

- 用 `MockBackend`（测试里实现 `ClipboardBackend`）验证 `copy` 双写：`read` 返回系统值、`paste_text` 优先系统；
- 系统 `read` 返回 `None`（如 `Osc52Backend`）时 `paste_text` 回退内部寄存器，内容一致；
- `backend.write` 返回 `false`（不可用）时 `copy` 不 panic，内部寄存器仍有值；
- `Auto` 选择：mock「arboard 失败」时回退到下一后端。

### 13.2 渲染快照

- 选区反显：对「选中中间几个字符」的 buffer 渲染，断言选中单元格 `modifier` 含 `REVERSED`、未选中单元格不含；
- 行级选区：断言整行（含行尾空白）均反显（`trailing_background` 填充）；
- 无选区时：overlay 清空，渲染输出与 `disabled_render_leaves_text_cells_default` 同口径（G9 回归闸）。

### 13.3 零分配测点（接 `tests/alloc_counting.rs`）

| # | 测点 | 目标 |
| --- | --- | --- |
| SL1 | 建立字符选区后稳态移动光标 ×100 | 0（overlay 容量复用） |
| SL2 | 有选区渲染一帧 | 仅 overlay 初次建容量一次性分配，稳态 0 |
| SL3 | 复制（Ctrl+C）一次 | 低频，允许 ≤ 2（`register` `String` 一次 + 系统后端一次）；纯内部后端则 1 |
| SL4 | 粘贴（Ctrl+V）一行 | ≤ 2（系统读 + 寄存器 clone 兜底 + 插入摊销）；纯内部后端则 1 |
| SL5 | 选区收起后渲染一帧 | 0，且与无选区数值一致 |

---

## 附录 A：改动点清单

| 文件 | 位置 | 改动 |
| --- | --- | --- |
| `src/text.rs` | 新增 | `Selection` / `SelectionMode`、`TextState::selected_text` / `delete_selection` / `insert_str`、`Selection::ordered` / `is_active` / `begin` |
| `src/lib.rs` | `11-13` | 加 `pub mod clipboard;`（顺序随意） |
| `Cargo.toml` | 依赖 | 加 `base64 = "0.22"`、`arboard = { version = "3", optional = true }`，`[features]` 加 `default = ["system-clipboard"]` / `system-clipboard = ["dep:arboard"]` |
| `src/clipboard.rs` | 新增 | `ClipboardBackend` trait、`ClipboardBackendKind`、`NativeBackend`（`Mutex<arboard::Clipboard>`）/ `Osc52Backend<W: Write>` / 退化内部、`Clipboard` 管理器（`copy` / `cut` / `paste_text`）、`Auto` 后端选择 |
| `src/widgets/editor.rs` | `204-222` | `EditorState` 增 `selection` / `clipboard` / `selection_dirty` 字段 |
| `src/widgets/editor.rs` | `224-256` | `load_file` 末尾 `selection_clear()` |
| `src/widgets/editor.rs` | `263-320` | `handle_key` 重构：`Motion` 枚举、Shift 扩展、Ctrl+C/X/V、Esc、替换语义、`after_edit` |
| `src/widgets/editor.rs` | 新增 | `selection_begin` / `selection_clear` / `recompute_selection_overlay` / `copy` / `cut` / `paste` / `paste_text` / `after_edit` |
| `src/widgets/editor.rs` | `32-38` | `handle_event` 增加 `Event::Paste` 分支 |
| `src/widgets/editor.rs` | `85-87` | `Editor::render` 在 `Viewport::render` 前按需重算 overlay |
| `src/action.rs` | `37-39` | 删除 `PaintDemo` 变体 |
| `src/app.rs` | `140-156` | `run` 转发 `Event::Paste` 到 `Editor` |
| `src/app.rs` | `285-300` 等 | 删除 `Action::PaintDemo` 的调用（`PaintDemo` 整体移除） |
| `tests/alloc_counting.rs` | 新增 | SL1–SL5 测点 |
