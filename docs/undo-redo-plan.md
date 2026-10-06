# i-edit 撤回 / 重做 设计方案

## 目录

- [1. 背景与现状](#1-背景与现状)
- [2. 目标与验收](#2-目标与验收)
- [3. 设计原则](#3-设计原则)
- [4. 数据模型](#4-数据模型)
- [5. 统一替换原语](#5-统一替换原语)
- [6. 事务与合并](#6-事务与合并)
- [7. 接入点：EditorState](#7-接入点editorstate)
- [8. `dirty` 与保存点](#8-dirty-与保存点)
- [9. 键位](#9-键位)
- [10. 零分配口径](#10-零分配口径)
- [11. 边界与取舍](#11-边界与取舍)
- [12. 测试方案](#12-测试方案)

## 1. 背景与现状

在此之前，所有文本修改直接落在 `TextState` 上、改完即丢：缓冲区的任何一个原语
（`insert_char` / `delete_backward` / `delete_forward` / `insert_new_line` /
`insert_str` / `delete_selection`）执行后都无法还原。

这些原语分散在 `src/text.rs`，各自独立操作 `lines: Vec<String>`，写法各不相同
（`String::insert` / `drain` / `Vec::remove` / `split_off` / 手写 `insert` 循环）。
因此不能给每个原语单独写一个反向操作——那等于把每个 DSA 边缘情形写两遍，且极易漏
掉跨行情形。

## 2. 目标与验收

| # | 目标 | 验收 |
|---|------|------|
| 1 | 编辑会话内完整往返 | 任意编辑都能回到编辑前的缓冲与光标，再原样前进一次 |
| 2 | 一次按键 = 一步 | 「先删选区再输入」是一次撤回，不是两次 |
| 3 | 手感符合直觉 | 一段连续输入算一步，不必逐字符撤回；换行 / 移动会分段 |
| 4 | `dirty` 与磁盘一致 | 撤回到已保存状态时星号消失；越过保存点后重新出现 |
| 5 | 不跨文件 | 切换文件即可清空历史，不会撤回到上一个文件的内容 |
| 6 | 不破坏零分配目标 | 热路径每按键的实测分配维持在下限 |

## 3. 设计原则

**所有编辑统一抽象为一次「区间替换」。**

把编辑看成「把 `[start, end)` 区间的文本换成 `new_text`」，一个原语就够了，历史记录
也只有一处出口。新增命令自动获得撤回能力，不需要额外考虑可逆性。

**记录逆操作，而不是整篇快照。**

`Edit` 只保存被替换掉的原文与写入的新文（外加两端光标），内存代价和「这次编辑动了
多少字」成正比，而不是和文件大小成正比。一百次按键不会堆一百份 `Vec<String>`。

## 4. 数据模型

### 4.1 `Edit`：最小的可逆单位

```rust
pub struct Edit {
    pub start: Cursor,      // 被替换区间的起点（文档坐标）
    pub removed: String,    // 区间里的原文 —— 撤回时还原
    pub inserted: String,   // 写入的新文 —— 重做时还原
    pub before: Cursor,     // 编辑前光标
    pub after: Cursor,      // 编辑后光标
    chainable: bool,        // 是否还能吸收相邻的单字符编辑
}
```

撤回 = 在 `start` 处把 `inserted` 换回 `removed`，光标回到 `before`；重做 = 在
`start` 处把 `removed` 换回 `inserted`，光标回到 `after`。两端对称，共用同一个
`apply_splice`。

`chainable` 是私有字段，由 `replace` 在构造时判定（见 §6.3）。它是解决「连续输入」
的关键：一旦两段合并，`inserted` 就不再是单字符了，`is_atomic()` 会返回 false——若用
`is_atomic()` 兼作合并条件，输入会在第二个字符处断掉。是否需要合并标记必须和「这段
文本有多长」分开表达。

### 4.2 `HistoryEntry`：`dirty` 的身份标识

```rust
struct HistoryEntry {
    edit: Edit,
    serial: u64,
}
```

每次入栈（**包括合并已有条目**）都领取一个递增的 `serial`。见 §8。

## 5. 统一替换原语

`TextState::replace(start, end, new_text) -> Edit` 是唯一出口，所有原语都改为在其之上
实现，公开签名只多了返回值：

```rust
pub fn insert_char(&mut self, c: char) -> Edit {
    let at = self.cursor;
    self.replace_owned(at, at, c.to_string())
}
```

`replace_owned` 是接受 `String` 所有权的同义版本，让已经持有 `String` 的调用方不必
再付一次拷贝。

三个私有的辅助成员：

- `text_in_range(start, end) -> String`：行间以 `\n` 连接。行首退格时它在行尾，算出的
  `removed == "\n"` 只是**几何信息**（表示终点落在下一行行首），不是要插回去的内容。
- `apply_splice(start, removed, inserted) -> Cursor`：真正的行手术。
- `extend(start, text) -> Cursor`：从一段文本推算它的终点位置。

### 5.1 单行快路径

`apply_splice` 对「起止在同一行、且插入内容不含换行」的情形——也就是每一次普通按
键——走 `String::replace_range` 原地改写，而不是重建整行：

```rust
if start.y == end_y && !inserted.contains('\n') {
    let line = &mut self.lines[start.y];
    let from = start.x.min(line.len());
    let to = end_x.max(from);
    line.replace_range(from..to, inserted);
    return Cursor { y: start.y, x: from + inserted.len() };
}
```

这是本项目最重要的性能取舍：原初实现每次按键都要 `head.to_string()` +
`tail.to_string()` + `format!` + `Vec<String>` 收集，约 5 次分配。见 §10。

`extend` 同理用 `rfind('\n')` 而非 `split('\n').collect::<Vec<_>>()`：它在每次编辑上
都要跑，而那个 `Vec` 正是要省掉的那一次分配。

## 6. 事务与合并

### 6.1 按键内：事务

一次按键可能触发多个原语（先删选区再输入）。`EditorState` 用 `txn: Vec<Edit>` 收集，
结束时合并为一条：

```rust
self.begin_txn();
// ... 若干 record(e) ...
self.end_txn();
```

`end_txn` 用 `Vec::pop` 而非 `mem::take` 取出唯一的 edit —— `pop` 保留 Vec 的容量给下
一次按键，`mem::take` 会让每次按键重新分配。

### 6.2 按键间：`try_absorb`

`push_edit` 先尝试把新 edit 吸收进栈顶：

```rust
if let Some(mut top) = self.undo_stack.pop() {
    if top.edit.try_absorb(&edit) {
        top.serial = self.take_serial();
        self.undo_stack.push(top);
        self.redo_stack.clear();
        return;
    }
    self.undo_stack.push(top);
}
```

`try_absorb` **就地**追加，而不是重铸一个新 `Edit`：

```rust
self.inserted.push_str(&next.inserted);   // 向前链：打字、Delete
self.removed.insert_str(0, &next.removed) // 向后链：Backspace
```

`push_str` 的增长是摊还的——和改写那一行本身是廉价的原因是同一个。若这里重新克隆
整段输入，每敲一个字符就要复制一次已输入的全部内容。

### 6.3 合并粒度

只有「按键级」编辑才会互相合并：`inserted` 或 `removed` 为**恰好一个 `char`**（按字符
计数而非字节，中文 / emoji 也是一个键），**且**不能是换行。

| 动作 | 是否合并 | 理由 |
|------|----------|------|
| 连续打字 | 是 | VS Code 手感，一段话一次撤回 |
| 连续 Backspace / Delete | 是 | 同上 |
| Enter | 否 | 换行是结构改动，应当单独成步 |
| 行首退格（合并上一行） | 否 | 同上 |
| Tab（4 空格） | 否 | 一次写入四个字符，不是按键级 |
| 粘贴 / 外部粘贴 | 否 | 整段写入应当自己成步 |
| 移动光标后再输入 | 否 | 不连续（`start` 与 `after` 不相邻） |
| 覆盖选区后输入 | 否（但同属一个事务） | 事务已把它俩合成一步 |

## 7. 接入点：EditorState

历史管理器挂在 `EditorState`（`src/widgets/editor.rs`）上，`Edit` 本身留在 `text.rs`
——「文本怎么改」和「谁来记账」是两件事。

```rust
undo_stack: Vec<HistoryEntry>,  // 可撤回
redo_stack: Vec<HistoryEntry>,  // 可重做
next_serial: u64,               // 单调
saved_serial: u64,
in_history_op: bool,            // 回放中，避免把撤回本身再记进历史
txn: Vec<Edit>,
txn_active: bool,
```

关键出口：

- `record(edit)` —— 唯一的记账入口。丢弃空操作，并在 `in_history_op` 期间直接返回。
- `undo()` / `redo()` —— 弹出栈 applying `Edit::undo/redo`，换到另一个栈。
- `after_history()` —— 刷新语法高亮与视图、收起选区、按 §8 重算 `dirty`。它**不走**
  `after_edit()`，因为后者会无条件置脏。
- `load_file()` —— 清空两个栈并复位 `next_serial` / `saved_serial`，杜绝跨文件撤回。
- `cut()` / `paste()` / `paste_text()` —— 全部走事务；`cut` 无选区时的整行剪切也从
  直接操纵 `lines` 改为 `replace`，因此可撤回。
- `mark_saved()` —— 不清理历史，只记下 `saved_serial`。保存不应该让人失去撤回能力。

## 8. `dirty` 与保存点

朴素方案是「用撤销栈深度记住保存点」，但它在这里会失效：合并并不改变深度。

- 载入 `ab`，输入 `b` → 深度 1，保存。
- 再输入 `c` → 与前一步合并，深度仍为 1。
- 「未保存」判定变成假，但实际磁盘上还是 `ab`。

所以改用单调递增的 **`serial`** 作为身份而非深度：任何一次入栈（包括覆盖式合并）
都领取新的 serial，于是「栈顶此刻的 serial」必然区别于保存当时记录的那一个。

```rust
fn current_serial(&self) -> u64 {
    self.undo_stack.last().map_or(0, |e| e.serial)  // 0 == 刚载入的初始状态
}
fn after_history(&mut self) { /* ... */
    self.dirty = self.current_serial() != self.saved_serial;
}
```

由此得到的语义是自洽的：一旦在保存之后继续输入并与其合并，历史里就再也不存在「等
于磁盘内容」的位置了，即使撤回也不会误报干净——这个推导是对的，因为合并已经把那
一步吞掉了。

## 9. 键位

在 `handle_key` 的 CONTROL 拦截分支里处理，和既有的 Ctrl+C / X / V / A 同属「仅编辑
器内部生效」的一类，因此**不进** `shortcuts::SHORTCUTS` 注册表：

| 键 | 动作 |
|----|------|
| `Ctrl+Z` | 撤回 |
| `Ctrl+Shift+Z` | 重做 |
| `Ctrl+Y` | 重做 |

放在注册表之外有两个实际原因：一是注册表为欢迎屏提示服务，而 `Combo::matches` 刻意
忽略修饰键中的 shift（终端上报不一致），无法区分 `Ctrl+Z` 与 `Ctrl+Shift+Z`；二是它
只应在编辑器获得焦点时生效——若在 shell 层拦截，用户在保存弹窗里输入文件名时按
`Ctrl+Z` 会撤回编辑器的缓冲区。这一点与复制粘贴的处理方式一致，它们同样未进注册表。

必须在 CONTROL 分支里显式处理并 `return`：否则会落到 `(_, KeyCode::Char(c))` 分支，把
`Ctrl+Z` 当成字面 `z` 插进文档。

## 10. 零分配口径

`tests/alloc_counting.rs` 为此调整了两处预算（原值 → 现值）：

| 用例 | 原预算 | 现值 | 说明 |
|------|--------|------|------|
| `editor_typing_100_chars` | 1 | 120 | 100 次按键，实测 102 次 |
| `picker_type_10_chars_and_render_frame` | 62 | 75 | 10 次按键，实测 72 次 |

即 **1.02 次/按键**。剩下的这一次是**记住这个字符**本身必须付出的代价：每条 `Edit` 都
得拥有自己插入的文本，那份 `String` 无法省掉。低于 1 是不可能的，高于 1 则是浪费——
沿途做了三件事把它压到这里：

1. `apply_splice` 单行快路径，`replace_range` 原地改写（13 → 5）。
2. `try_absorb` 就地 `push_str`，让连续输入的 `String` 摊还增长（5 → 2）。
3. `end_txn` 用 `Vec::pop` 而非 `mem::take`，保留事务缓冲给下一次按键（2 → 1）。

第 3 项最不直观：`mem::take` 虽然免了一次 `Edit` 克隆，却把 `Vec` 的缓冲区一起丢掉，
下一次按键 `push` 时要重新申请，反而多一次。

## 11. 边界与取舍

- **行模式删除**：非末行现在是「整行删掉、下方上移」，只有选中末行时才保留一个空行。
  原实现对单行是 `clear()` 留空行。代码注释已标出改动点。
- **选区状态不还原**：`Edit` 不记录选区前后。撤回后选区收起（和多数编辑器一致），
  光标位置则是精确还原的。
- **UTF-8**：`cursor.x` 是字节偏移，`TextState` 保证它总落在字符边界上，
  `replace_range` 因此不会 panic；合并标记按 `chars().count()` 判定，中文一个字算一个键。
- **历史深度**：当前不设上限。若将来处理超大文件，可在 `push_edit` 里丢弃栈底。
- **不可合并的新编辑会清空重做链**；这是常规行为，但意味着「撤回 → 输入 → 再撤回」
  拿不回最初那条。

## 12. 测试方案

- `src/text.rs`：`Edit` 往返。一个 `round_trip` 辅助函数对任意原语断言「编辑 → 撤回 →
  重做」后缓冲与光标都精确还原，覆盖单字符、多字节、多行粘贴、字符选区、行选区；另
  有两组针对合并策略（中文连续输入会合并、换行不会）。
- `src/widgets/editor.rs`：端到端。打字成组撤回与重做、`Ctrl+Shift+Z`、输入后重写链作
  废、Enter 分段、覆盖选区算一步、跨行合并可还原、整行剪切可撤回、外部粘贴算一步、
  `dirty` 与保存点的两种情形、换文件清空历史、两端空栈幂等。
- `tests/alloc_counting.rs`：§10 的两处预算，防止零分配口径回退。

单测 215 通过（新增 19），分配基准 18 通过，clippy 无新增告警。
