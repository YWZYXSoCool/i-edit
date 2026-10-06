# i-edit 分配压缩方案

> 状态：草案 v1 ｜ 日期：2026-10-06 ｜ 基于当前工作区源码的静态分析，行号以当时版本为准

**范围与口径**：仅统计 `src/` 下由本项目代码触发的堆分配（`String` / `Vec` / `PathBuf` / `HashMap` / `HashSet` / `clone` / 格式化等）。
ratatui、crossterm、tui-logger、log、color-eyre 内部实现产生的分配不计入，但相关调用点会注明。

## 目录

- [1. 背景与盘点摘要](#1-背景与盘点摘要)
- [2. 目标与验收标准](#2-目标与验收标准)
- [3. 明确接受项](#3-明确接受项)
- [4. Phase 0：基线测量](#4-phase-0基线测量)
- [5. Phase 1：渲染路径零 String](#5-phase-1渲染路径零-string)
- [6. Phase 2：状态与派生数据](#6-phase-2状态与派生数据)
- [7. Phase 3：I/O 路径](#7-phase-3io-路径)
- [8. Phase 4：日志与收尾](#8-phase-4日志与收尾)
- [9. 每帧剩余的小 Vec](#9-每帧剩余的小-vec)
- [10. 风险与回归清单](#10-风险与回归清单)
- [11. 提交切分与顺序](#11-提交切分与顺序)
- [附录 A：热点排行](#附录-a热点排行)
- [附录 B：按模块分配点清单](#附录-b按模块分配点清单)

---

## 1. 背景与盘点摘要

- 全项目直接分配点约 80 处；**无 `Box` / `Rc` / `Arc`**；核心编辑与视口渲染已刻意低分配（`FixedBuf`、借用式 `Viewport`、`String::from_utf8(buf)` 复用读缓冲、`mem::take` 转移所有权）。
- 问题集中在两类：
  1. **每帧渲染路径**上的 `format!` / `to_string`：状态栏、输入框、picker、文件树、命令面板、消息框；
  2. **每次按键路径**上的全量重建与克隆：`file_tree::visible_rows()`、`picker::highlighted_row().cloned()`、`utils::find_best_char_position` 的逐字符 `to_string`。
- I/O 路径两处结构性浪费：`list_dir` 排序时每次比较 2 个 `to_lowercase()`；`write_text_file` 的 `join` 之后紧跟尾部换行导致的重分配。
- 事件管道 `Actions::drain()`（`mem::take`）每轮丢掉组件 outbox 的容量，导致下一轮 emit 重新分配。
- `main.rs` 把日志级别开到 Trace，使 `App::apply` 的每条 action 都触发第三方 logger 的格式化分配。
- 热点排序见[附录 A](#附录-a热点排行)，逐文件清单见[附录 B](#附录-b按模块分配点清单)。

## 2. 目标与验收标准

**目标**：消除"每次按键 / 每帧"路径上由本项目代码触发的堆分配；I/O 路径降至 O(n) 必需量；功能、键位与公开行为不变。

验收方式：`tests/alloc_counting.rs` 中的计数分配器断言（见 Phase 0）。

| 操作                                | 现状（估算，本项目代码）              | 目标                        |
| ----------------------------------- | ------------------------------------- | --------------------------- |
| 编辑器输入一个字符后的整帧          | 10+                                   | 仅少量 `Line` Vec（见 §9）  |
| 编辑器方向键 / Ctrl+←→              | 每字符 1 String；每按键 1 Vec         | 0                           |
| 打开文件后渲染一帧（树展开 100 行） | ≈ 300+                                | ≤ 行数级的小 Vec            |
| 文件树 ↑ / ↓                        | 1–2 次全量克隆（每行 PathBuf+String） | 0                           |
| 文件树展开一个目录                  | 2 PathBuf + 全量克隆 + `list_dir`     | 1 PathBuf + `list_dir`      |
| picker 键入一个字符                 | PathBuf + 全新 rows Vec + `list_dir`  | `list_dir`（rows 复用容量） |
| 保存一次（1k 行文件）               | `join` + realloc + PathBuf            | 一次精确分配                |
| 每条 action                         | 日志格式化分配                        | 0（日志降级）               |

## 3. 明确接受项（不压缩）

- `read_text_file` 每行一个 `String`：`Vec<u8>` → `String` 所有权转移的固有代价；
- `DirEntry` 每项一份 `PathBuf` + `String`；
- 每条 `Message` 的 `String`（上限 `MAX_MESSAGES = 4`）；
- 行 `String` 与 `lines` / `actions` / `rows` 等容器的摊销增长；
- 第三方 crate 内部（ratatui `Line` 构造、`Block`、crossterm 事件对象、tui-logger 记录等）。

---

## 4. Phase 0：基线测量

新增 `tests/alloc_counting.rs`（独立测试二进制，可合法声明 `#[global_allocator]`）：

```rust
struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    // alloc / realloc 累加，dealloc 可选累减
}

#[global_allocator]
static A: Counting = Counting;

static SERIAL: Mutex<()> = Mutex::new(());

/// 取锁、读计数、执行、返回（结果, 分配次数差值）。
fn measure<R>(f: impl FnOnce() -> R) -> (R, usize);
```

测点（记录 before 数字，写进测试注释，之后作断言）：

1. 编辑器输入字符 / 方向键 / Ctrl+←→（各 100 次取均值）；
2. 打开 1k 行文件后渲染一帧（文件树关闭 / 展开 100 行两档）；
3. 文件树 ↑/↓ 各 100 次、展开一次目录；
4. picker 键入 10 个字符、渲染一帧（30 行目录）；
5. 命令面板渲染一帧；
6. 保存一次 1k 行文件；
7. `App::apply` 一条常见 action。

注意事项：测试 harness 与其它测试线程也会分配，用互斥锁串行；断言写成"≤ N"而非精确相等；必要时用 `--test-threads=1` 复核。

---

## 5. Phase 1：渲染路径零 String

> 共同手法：`format!` / `to_string` 改为借用 span（`Span::styled` 接受 `impl Into<Cow<str>>`）；
> 文本裁剪交给 `Line::render`，或改为返回 `&str` 切片的裁剪函数；缩进改用直接写 `buf`。
> 每帧只剩下 `vec![Span]` 的小 Vec，见 §9 的进一步讨论。

- **P1.1 `src/utils.rs:9` —— 字符宽度零分配。**
  `UnicodeWidthStr::width(c.to_string().as_str())` → `UnicodeWidthChar::width(c).unwrap_or(0)`（与 `src/lib.rs:24` 的写法统一）。
  这是每次 ↑/↓/PageUp/PageDown 的逐字符分配（经 `text.rs:140` 的 `move_to_line`）。

- **P1.2 `src/widgets/input.rs:136-208` —— 输入框渲染零 String。**
  - `display()` 改为返回 `(Cow<'t, str>, usize)`：非密码模式 `Cow::Borrowed(text)`；密码模式保持 `"*".repeat(...)`（该分支分配无法避免，且极少用）。
  - `before_cursor.to_string()` → `Span::styled(&display_text[..cursor_pos], …)`。
  - `cursor_char.to_string()` → `let mut buf = [0u8; 4]; cursor_char.encode_utf8(&mut buf)`。
  - `after_cursor.chars().skip(1).collect::<String>()` → `&after_cursor[cursor_char.len_utf8()..]`。
  - placeholder 分支直接用 `placeholder`（本身就是 `&'a str`），删除 `.to_string()`。
  - 预期：每个输入框每帧 4 个 String → 0。

- **P1.3 `src/widgets/status_bar.rs:134-200` —— 状态栏零 String。**
  - `position_info` → `FixedBuf<64>`（"Ln " 3 + usize 最长 20 位 + ", Col " 7 + 20 = 50 ≤ 64），`write!` 栈上格式化。
  - 删除 `left_info` 的 String 构建，改为在 `render` 内拼 `Line`：文件图标、名称、`" [+]"`、键盘图标段各自一个借用 span；`key_info()` 的 `FixedBuf` 保持。
  - 右侧宽度：`UnicodeWidthStr::width(position.as_str()) + 2`（图标 1 列 + 空格 1 列，图标单列已由 `icon.rs` 测试保证）。
  - 给 `FixedBuf` 增加 `impl PartialEq<&str>`，现有 `position()` 测试断言可不改；`left_info` 的 4 个测试改为"把 Line 的 span 连接成 String 再断言"（测试内分配无所谓）。
  - 预期：每帧 3 个 String + 追加 realloc → 0 String。

- **P1.4 `src/widgets/message_box.rs:209-213` —— 消息行零 String。**
  `format!(" {}", message.text)` → `Span::raw(" ")` + `Span::styled(message.text.as_str(), …)`。
  预期：每条消息每帧 1 String → 0。

- **P1.5 `src/widgets/command.rs:178-186, 269, 308` —— 命令面板。**
  - 建议列表：`self.suggestions = …collect()` → `self.suggestions.clear(); self.suggestions.extend(…)`，容量跨按键复用。
  - 前缀匹配改为零分配比较：对 `name.as_bytes()` 取 `..input.len()` 前缀，用 `eq_ignore_ascii_case(input.as_bytes())`（目录内命令名全为 ASCII，行为等价；补大小写/非 ASCII 输入的对照测试）。
  - 占位符 `format!` → `static OnceLock<String>`，首次渲染构造一次；或存入 `CommandPaletteState` 的 `open()`。
  - 每行建议的 `format!(" {marker} {} · {}", …)` → 借用 span 组合（marker / 空格 / name / `" · "` / description）。
  - 预期：打开期间每帧 1 + ≤6 个 String → 0。

- **P1.6 `src/widgets/picker.rs:556-592, 441` —— 列表行与标题零 String。**
  - `row_text()`（String）+ `clip_to_width()`（String）→ `row_spans(row, max_width) -> Vec<Span>`：marker / glyph / 空格借用，**name 用 `fn clip_to_width(&str, usize) -> &str` 切片**（宽字符边界安全、零分配）；`Row::Parent` 同法。
  - 标题 `format!` → `Line::from(vec![Span::raw(icon), Span::raw(" "), Span::raw(mode.title())])`。
  - 预期：每可见行每帧 2 String → 0。

- **P1.7 `src/widgets/file_tree.rs:303, 353, 393-399` —— 树渲染零 String。**
  - `render` 开头删除 `state.root.clone()`：先渲染行，最后再 `state.root.as_deref()` 画提示（调整借用顺序即可）。
  - 提示 `root.display().to_string()` → `root.to_string_lossy()` 的 `&str` 借用（UTF-8 时零分配）。
  - `render_row` 的 `"  ".repeat(row.depth)` + `push_str` → 缩进直接写 `buf` 空格，其余用 marker / glyph / 空格 / name 四个借用 span。
  - 预期：每帧 1 root clone + 1 hint String + 每行 1–2 String → 0 String。

- **P1.8 `src/widgets/editor.rs:153-162` —— 欢迎屏。**
  `welcome_text()` 每帧 collect → `static WELCOME: OnceLock<Vec<Line<'static>>>`，之后只借引用。

**Phase 1 验收**：`cargo test` 全绿；渲染相关测点不再出现 String 分配（仅允许小 Vec）。

---

## 6. Phase 2：状态与派生数据

### P2.1 文件树：合并映射 + 行缓存（重点收益）

`src/widgets/file_tree.rs` 重构为：

```rust
struct Dir {
    children: Option<Vec<DirEntry>>, // None = 从未加载过（含"读取失败缓存为空"语义）
    expanded: bool,
}

pub struct FileTreeState {
    root: Option<PathBuf>,
    dirs: HashMap<PathBuf, Dir>, // 替代 children + expanded，每目录 1 个 PathBuf
    rows: Vec<Row>,              // 派生缓存，只在结构变化时重建
    selected: usize,
    scroll: usize,
    actions: Actions,
}
```

- `rebuild_rows()`：沿用现有显式栈展开逻辑，`self.rows.clear()` 后填充（**容量跨调用复用**）；`open_root()` / `set_expanded()` 末尾各调用一次。
  `root` 用 `as_deref()` 借用即可，与 `self.rows` / `self.dirs` 是字段级不相交借用，无需克隆。
- 按键处理（`move_selection` / `expand_or_step_in` / `collapse_or_step_out` / `activate` / `clamp_selection`）全部改为读 `self.rows`；需要修改时先取出 `path: PathBuf`（`clone()` 一次，仅展开 / 回车路径），再调 `&mut self` 方法。
- `render` 直接迭代 `state.rows`；先算 `row_count` 再调 `keep_selection_visible(&mut …)`，随后只读迭代。
- 对外暴露 `pub fn rows(&self) -> &[Row]`；测试中 `visible_rows()`（8 处 + `row_names` helper）机械替换；直接访问 `children` / `expanded` 字段的 6 处断言改为查询 `dirs`（测试在模块内，可直接访问）。
- 预期：文件树 ↑/↓ 变为 0 分配；展开 / 回车从"1–2 次全量克隆 + 每帧再来一次"降为"仅结构变化时一次重建"；每目录 PathBuf 从 2 个降为 1 个。

### P2.2（可选进阶）`Arc<DirEntry>` 消除重建克隆

`list_dir` 返回 `Vec<Arc<DirEntry>>`；`Row { entry: Arc<DirEntry>, depth, expanded }`。
重建只做引用计数 + Vec 复用，**重建也近乎零分配**；picker 的 `Row::Entry(Arc<DirEntry>)` 同步受益（`highlighted_row` 克隆降为 refcount）。
代价：`entry.path` 需 `clone()` 而非移动、`fs.rs` API 变化、测试字段访问调整。建议 P2.1 稳定后作为独立提交。

### P2.3 事件管道：动作队列稳态零分配

- `Actions` 增加 `pub(crate) fn take_into(&mut self, out: &mut Vec<Action>) { out.append(&mut self.0) }`（`append` 转移元素、**保留源容量**）；`drain()` 保留给测试与公开 API。
- 各组件增加 `take_actions_into(&mut self, out)`；`PopupState` 版本依次 append 自己的 / command / picker 的。
- `App` 增加常驻 `actions_buf: Vec<Action>`；`apply_actions`：`clear()` → 各组件 `take_into` → `reverse()` 后 `while let Some(action) = pop()`（保持原顺序，且不产生 `&mut self` 借用冲突）。

### P2.4 编辑核心

- `src/text.rs:191-273`：`move_word_left` / `move_word_right` 去掉 `char_indices().collect::<Vec<_>>()`，用 `line[..pos].char_indices().next_back()`（`CharIndices` 是 `DoubleEndedIterator`）双向扫描；`prev_char_boundary` / `next_char_boundary` 顺带用同一原语。
- `src/widgets/input.rs:36-47`：`set_text` 复用内部唯一一行（`lines[0] = text.into(); lines.truncate(1)`，必要时 push）；`clear` 用 `lines.clear(); lines.push(String::new())` 保留 Vec 容量。

### P2.5 picker 与 app 的所有权整理

- `picker.rs:333`：`highlighted_row(&self) -> Option<&Row>`；各 handler 先取所需（`path.clone()` 或 `name.clone()`，1 次分配）再调 `&mut` 方法（现为 `DirEntry` 全克隆 ≥2 次）。
- `picker.rs:374`：`refresh` 改 `self.rows.clear()` + push/extend（容量跨刷新复用，消除每次按键的新 Vec）。
- `picker.rs:339`：删除 `trim().to_owned()`，直接 `dir.join(self.name_input.text().trim())`。
- `app.rs:270`：`load_file_now(&mut self, path: PathBuf)`，先取 `last_dir` 与消息，再 `load_file(path, lines)` 移动；省 1 PathBuf / 次。
- `app.rs:287-303`：`open_folder` 的 2 次 `path.clone()` 减为 1 次（`open_root(path.clone())`，`last_dir = Some(path)`）。
- 低频路径（`save_current` / `resolve_confirm` 的 `path.clone()`）保留，I/O 开销占绝对主导。

**Phase 2 验收**：文件树导航 0 分配；编辑移动 0；动作管道稳态 0；既有测试通过（除机械性调整）。

---

## 7. Phase 3：I/O 路径

- **P3.1 `fs.rs:131-136` —— 排序零分配。**
  排序比较器内的 `to_lowercase()`（每次比较 2 个 String，O(n log n) 次）改为零分配比较器：用 `a.chars().flat_map(char::to_lowercase)` 与 `b` 交错比较（`char::to_lowercase` 返回迭代器，不分配）。
  备注：与 `str::to_lowercase` 仅在希腊词尾 sigma 等边缘情形不同，对文件名排序无实际影响。

- **P3.2 `fs.rs:97-103` —— 写文件一次精确分配。**
  `lines.join("\n")` 后再 `push('\n')` 会触发 realloc；改为 `String::with_capacity(总长 + 行数)` 后逐行 `push_str` + `push('\n')`，一次分配、一次写入。

- **P3.3 `fs.rs:45-94` —— 读文件（保持设计）。**
  保留每行 `Vec<u8>` → `String` 的移动设计（接受项）；可选：`lines.reserve(估计值)` 减少外层 Vec 增长。不改变"不整文件驻留"的内存目标。

- **P3.4 `fs.rs:152`（可选）—— `expand_path` 返回 `Cow<Path>`。**
  绝对路径（picker 的常态）直接借用，省每次 `refresh` 的 `to_path_buf()`；相对路径分支可用 `OnceLock` 缓存 `current_dir()`（本应用不 chdir，需在文档注明前提）。

---

## 8. Phase 4：日志与收尾

- **P4.1 日志降级。**
  `app.rs:162` 每条 action 的 `info!` 降为 `debug!`；`popup.rs` 的开 / 关日志保持 `info!`；`main.rs:7-8` 默认级别改为 `Info`，或用 `I_EDIT_LOG` 环境变量恢复 Trace。
  这是"调用点在我们代码、分配在第三方内部"的唯一大头。
- **P4.2 收尾。**
  把 §2 的验收指标固化为 `tests/alloc_counting.rs` 断言；在模块文档注明分配政策（接受项 vs 必须零分配项）。

---

## 9. 每帧剩余的小 Vec

Phase 1 之后，`Line::from(vec![…])` 中的 `vec!` 是**本项目代码**每帧仅剩的分配来源（`Line` 内部存 `Vec<Span>`）。两个处理层级：

1. **接受**：数量 = 渲染的"多 span 行数"（状态栏 2、树 R 行、picker V 行……），都是小 Vec，比 String + 克隆低一个数量级；
2. **进阶（可选，独立验证）**：ratatui 0.30 的 `Span` 实现了 `Widget`，把 `Line::from(vec![…]).render(area, buf)` 换成按列偏移直接 `Span::render`，每帧归零。
   需先验证宽字符右边缘裁剪与高亮背景填充行为，并补测试后再动。

## 10. 风险与回归清单

| 风险                                          | 对策                                                                                                       |
| --------------------------------------------- | ---------------------------------------------------------------------------------------------------------- |
| 行尾宽字符裁剪行为变化                        | `clip_to_width` 先改为 `&str` 切片版（保证不切字符）；加"中文文件名贴右缘"测试                             |
| 密码模式光标偏移                              | `Cow` 分支保留现有 chars 计数逻辑，`password_mode_masks_one_star_per_char` 已覆盖                          |
| 文件树重构改动面大                            | 单独提交；测试中 14 处字段 / helper 访问机械替换；外部调用 `set_expanded`（测试 L701）仍触发重建，行为不变 |
| `FixedBuf<64>` 截断                           | 容量按 usize 最大值推导（50 字符），加边界测试                                                             |
| `eq_ignore_ascii_case` 与 `to_lowercase` 差异 | 补测试：全大写 / 混合大小写 / 非 ASCII 输入与旧行为一致（目录全 ASCII）                                    |
| 日志降级影响排障                              | 环境变量恢复 Trace；生命周期事件保留 `info!`                                                               |
| `Arc<DirEntry>`（P2.2）公开 API 变化          | 独立提交、单独评审；不做不影响其它收益                                                                     |

**不做的事**：不引入自定义 / 池化全局分配器；不把 `Arc` 引入渲染热路径（P2.2 仅限数据层）；不改变任何按键、消息文本与文件行为；不修改第三方 crate。

## 11. 提交切分与顺序

1. `test: add allocation counting harness and baselines`（Phase 0）
2. `perf: render paths without heap strings`（Phase 1，约 250 行）
3. `perf: cache file tree rows and merge directory maps`（P2.1，约 200 行）
4. `perf: reuse action queues and remove per-key allocations`（P2.3–P2.5，约 150 行）
5. `perf: single-allocation file writes, allocation-free dir sort, log level`（Phase 3–4，约 80 行）

可选项（P2.2、P3.4、§9.2）各自独立提交。

### 度量记录

测试：`tests/alloc_counting.rs`（线程本地分配差值，避免 libtest 其它线程噪声；`cargo test --test alloc_counting -- --nocapture` 可复现原始值）。

- **before** = `HEAD`（`git stash push -- src` 后运行；7b 因 HEAD 缺少 `apply_for_test` 而不可测）。
- **after** = Phase 1–4 全部改动落地后（含 P4.2 收紧的断言）。
- 保存测点为 3 次取最小值（Windows I/O 抖动）；“提交”列对应上文 1–5 的切分计划。

| 测点                                               | before                  | after                                                 | 提交  |
| -------------------------------------------------- | ----------------------- | ----------------------------------------------------- | ----- |
| 1a 编辑器输入字符 ×100                             | 1（0.01/键）            | 1（0.01/键，摊销增长 §3）                             | 4     |
| 1b 编辑器 ↓ ×100                                   | 4600（46/键）           | 0                                                     | 2     |
| 1c Ctrl+← ×100                                     | 300（3/键）             | 0                                                     | 4     |
| 1d Ctrl+→ ×100                                     | 300（3/键）             | 0                                                     | 4     |
| 2a 1k 行编辑器渲染一帧 80×24（树关闭）             | 0                       | 0                                                     | 2     |
| 2b 文件树渲染一帧（根 100 子项）                   | 328                     | 24（每可见行 1 个 `Line` Vec，§9.1）                  | 3     |
| 3a 文件树 ↓ / ↑ ×100                               | 21000 / 21000（210/键） | 0 / 0                                                 | 3     |
| 3b 展开子目录（冷路径）                            | 210                     | 137（`list_dir` 每项 PathBuf+String，§3）             | 3     |
| 4 picker 键入 10 字符 + 渲染一帧（30 项）          | 71                      | 62（行文本/容器增长）                                 | 2 / 4 |
| 5 命令面板渲染一帧                                 | 31                      | 8（每建议行 1 个 span Vec，§9.1）                     | 2     |
| 6 保存 1k 行（3 次取 min）                         | 3                       | 2（内容 String + Windows 写路径）                     | 5     |
| 7a 动作队列稳态（公开 `drain` 路径，1000 轮）      | 3000（3/轮）            | 1000（1/轮；内部 `take_into` 已消除，公开路径不可达） | 2 / 4 |
| 7b `App::apply` 单条 action ×100（ToggleFileTree） | —（HEAD 无测试钩子）    | 0                                                     | 4     |

---

## 附录 A：热点排行

| #   | 位置                                            | 触发                             | 分配内容                                                     |
| --- | ----------------------------------------------- | -------------------------------- | ------------------------------------------------------------ |
| 1   | `src/widgets/file_tree.rs:214` `visible_rows()` | 文件树按键 1–2 次/键 + 每帧 1 次 | 每次重建整个可见列表，逐行克隆 `PathBuf`+`String` + Vec 扩容 |
| 2   | `src/utils.rs:9`                                | 每次 ↑/↓/PageUp/Down             | 行内每个字符一个临时 `String`                                |
| 3   | `src/widgets/input.rs:136-208`                  | 每帧、每个输入框                 | 3–4 String + 1 Vec / 帧                                      |
| 4   | `src/widgets/picker.rs:333, 529, 556-592`       | 确认 / 补全按键；每可见行每帧    | 克隆 `DirEntry`；`row_text`+`clip_to_width` 2 String / 行    |
| 5   | `src/widgets/status_bar.rs:134-200`             | 每帧                             | 3 String + 追加 realloc                                      |
| 6   | `src/fs.rs:57, 134`                             | 每次读文件 / 列目录              | 每行 ≥1 Vec；排序每次比较 2 个 `to_lowercase`                |
| 7   | 所有 `info!` / `debug!`（如 `src/app.rs:162`）  | 每条 action、每次弹窗开关        | 第三方 logger 的格式化记录（Trace 全开）                     |
| 8   | `src/text.rs:201, 244`                          | Ctrl+← / Ctrl+→                  | 每次一个 `Vec<(usize, char)>`                                |

## 附录 B：按模块分配点清单

**核心编辑**

- `src/lib.rs`：无（`Cursor::display_col` 用 `UnicodeWidthChar::width`，不分配）。
- `src/utils.rs:9`：逐字符 `to_string`（A#2）。
- `src/text.rs`：`vec![String::new()]`（L27、L48-50）；`lines.push(String::new())`（L62）；`insert` / `push_str` 摊销重分配（L72、L89、L103）；`split_off` 每次 Enter（L109）；`lines.insert`（L113）；`collect::<Vec>`（L201、L244）。
- `src/action.rs`：`Actions` 的 `push`（L67）；`drain` 丢容量（L76）；携带 `PathBuf` 的 `Action` 变体构造 / 克隆（`command.rs:163` 当前全为 unit 变体，属潜在点）。

**渲染 / Widget**

- `src/widgets/viewport.rs`：零（`FixedBuf` 栈上）。
- `src/widgets/editor.rs:153-162`：`welcome_text()` 每帧 Vec（仅空 scratch）。
- `src/widgets/input.rs`：`set_text` 的 `vec![text.into()]`（L37）；`clear` 重建 `TextState`（L46）；密码 `"*".repeat`（L139）；非密码 `text.to_string()`（L143）；渲染 `before` / `cursor_char` / `after` / `placeholder` 的 String（L184、L191、L194、L205）；`Line::from(vec![…])`（L198）。
- `src/widgets/status_bar.rs`：`format!`（L135、L158、L190）；`push_str` / `write!` 追加 realloc（L138、L143）。
- `src/widgets/message_box.rs`：`Message.text: String`（L58）；`insert(0, …)`（L90）；渲染 `vec![…]` + `format!(" {}", text)`（L209-212）。
- `src/widgets/command.rs`：`to_lowercase`（L178）；`collect` 建议列表（L181-186）；Tab 补全 `set_text`（L199）；占位符 `format!`（L269）；每行建议 `format!`（L308）。
- `src/widgets/picker.rs`：`set_text(...to_string_lossy)`（L129、L146、L290）；`parent.to_path_buf()`（L283）；`with_separator`（L305、L597）；`entry.path.to_string_lossy().into_owned()`（L310）；`highlighted_row().cloned()`（L333）；`trim().to_owned()`（L339）；`expand_path` + `refresh` 的 rows Vec（L372-384）；标题 `format!`（L441）；`row_text` + `clip_to_width`（L529、L556-592）。
- `src/widgets/file_tree.rs`：`root.clone()`（L76）；`set_expanded` 双 PathBuf（L202、L205）；`visible_rows` 的 Vec + 逐行克隆（L219-244）；`root.clone()` 每帧（L303）；提示 `to_string`（L353）；`"  ".repeat` + `push_str`（L393-397）；`root_name`（L404-409）。
- `src/widgets/popup.rs`：`current_dir` + `PathBuf::from(".")`（L141）；标题 `format!`（L286、L307）；`take_actions` 的 extend（L122-124）。

**I/O**

- `src/fs.rs`：`BufReader::new`（L51）；`lines` Vec（L52）；每行 `Vec::new()` + `read_until` 增长（L57-58）；`String::from_utf8`（L84，复用无新增）；`join` + `push('\n')`（L98-99）；`entries` Vec（L111）；`entry.path()`（L115）；`to_string_lossy().into_owned()`（L121）；排序 `to_lowercase`（L134）；`PathBuf::new`（L157，无分配）；home / cwd / `to_path_buf` 各分支（L164-180）；`var_os` + `PathBuf::from`（L185-189）。

**Shell / 事件**

- `src/app.rs`：启动时 4 个 `TextState::default` 小 Vec（`App::new`）；`apply_actions` 聚合 extend（L147-152）；`info!`（L88、L162）；picker 打开路径的 `clone` / `to_path_buf` / `current_dir`（L222、L228-235）；`load_file_now` 的 `to_path_buf` + 消息 `format!`（L275-282）；`open_folder` 双 `clone` + 消息（L291-302）；`save_current` / `write_buffer` / `resolve_confirm` 的 clone、`to_path_buf`、消息（L307、L319-327、L360）。
- `src/main.rs`：logger / color-eyre 初始化均属第三方；代码本身无分配。
