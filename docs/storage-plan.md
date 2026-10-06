# i-edit storage 模块方案

> 状态：草案 v2（Phase 0–2 已落地）｜ 日期：2026-10-06 ｜ 基于当前工作区源码，行号以当时版本为准

**已落地**：Phase 0（模块骨架）、Phase 1（`App` 持有 `Storage`、MRU/面板开关入库）、Phase 2（会话恢复：文件 + 光标 + 文件夹 + 树展开）。
**未落地**：Phase 3（文件树列表走 `storage.cache`）、Phase 4（MRU 的 UI、日志迁移）。

## 目录

- [1. 定位与职责边界](#1-定位与职责边界)
- [2. 目录布局](#2-目录布局)
- [3. 四个 section](#3-四个-section)
- [4. 文件格式](#4-文件格式)
- [5. 模块结构](#5-模块结构)
- [6. API 与写入策略](#6-api-与写入策略)
- [7. 缓存策略](#7-缓存策略)
- [8. 失败与降级口径](#8-失败与降级口径)
- [9. 接入点（现状 → storage）](#9-接入点现状--storage)
- [10. 落地阶段](#10-落地阶段)
- [11. 零分配口径](#11-零分配口径)
- [12. 风险与未决项](#12-风险与未决项)
- [13. 测试](#13-测试)

---

## 1. 定位与职责边界

**storage 是 IDE 的"记忆层"：唯一负责"进程之外还有什么"的模块。** 一件事要跨进程存活，或要跨多次计算复用，就归它管。

| 归 storage | 不归 storage |
| --- | --- |
| 设置（用户可改、长期有效） | 文档内容（归 `fs`，那是用户文件不是编辑器状态） |
| UI 状态（面板开关、最近目录） | 编辑缓冲区 / undo 栈（内存态，进程内） |
| 会话（上次打开的文件、光标、树展开） | 语法高亮 token（归后续 `color-rendering` 的 highlight 缓存，若需落盘再开 section） |
| 缓存（目录列表等可再生数据） | 渲染派生数据（`Viewport`、`rows`） |

三条硬边界：

1. **文档 I/O 不经过 storage。** `fs::read_text_file` / `write_text_file` 是用户文件的读写通道，storage 只是它的调用方之一——存"刚打开过哪些文件"可以，存"文件内容"不行。
2. **storage 不认识 UI。** 它不知道 `Editor` / `FileTree` / 弹窗的存在，只提供 `Config` / `State` / `Session` / `Cache` 四类纯数据。谁读谁写由 `App` 决定。
3. **storage 不做决策。** 它不判断"该不该恢复会话"，只提供 `session.last_file`；`restore_session` 的语义由调用方解释。

---

## 2. 目录布局

```
$I_EDIT_HOME                    # 覆盖一切，测试与便携安装用
├── config                      # 用户设置
├── state                       # UI 状态 + MRU
├── session                     # 上次会话（缓冲区、光标、展开）
└── cache/
    └── dirs                    # 目录列表缓存
```

未设 `I_EDIT_HOME` 时的落点：

| 平台 | 根目录 |
| --- | --- |
| Windows | `%APPDATA%\i-edit` |
| 其他 | `$XDG_CONFIG_HOME/i-edit`，否则 `~/.config/i-edit` |

`paths::root()` 返回 `None`（无 HOME、无 APPDATA）时 storage **降级为纯内存**：读写都正常，只是不落盘。启动不失败。

缓存没有单独走 XDG cache 目录，而是放在根下的 `cache/`：一个目录好备份、好删、好 `I_EDIT_HOME` 整体迁移；代价是不符合 XDG 的分类洁癖，可接受（个人用途 IDE）。

---

## 3. 四个 section

| Section | 文件 | 生命周期 | 何时写 | 内容 |
| --- | --- | --- | --- | --- |
| `Config` | `config` | 永久 | 设置变更 | `restore_session`、`persist_cache` |
| `State` | `state` | 永久 | MRU / 开关变更 | `file_tree_visible`、`last_dir`、**三个 picker 各自的起始目录**、最近文件（20）、最近目录（10） |
| `Session` | `session` | 单次运行 | 切 buffer / 光标移动 / 树展开 | `last_file`、`last_folder`、每文件光标（200）、展开目录（500） |
| `Cache` | `cache/dirs` | 直到过期 | 列表变更（且 `persist_cache`） | 目录列表 + 读取时的 mtime（256） |

**文件夹比文件多一件事**：`session.set_folder()` 会同时清空展开集合——树切 root 时本来就会丢掉自己的缓存，带着旧 root 的目录只会恢复出一堆不在树里的路径。

**三个弹窗各记各的起始目录**（`PickerKind::OpenFile` / `OpenFolder` / `SaveAs`）：打开文件常年在 `src/`，打开文件夹常在项目根，另存又是一个地方，共用一个目录会让它们互相打架。目录在弹窗确认时记录（打开文件记其父目录，打开文件夹记它本身，另存记写入的父目录），下次打开该弹窗时优先用它。

优先级：`picker_dir(kind)` → 文件树 root → `state.last_dir` → cwd。**每一级都校验 `is_dir()`**：记住的目录被删掉或改名后，不能让弹窗开在一个读不出来的列表上。

**为什么分四个文件而不是一个**：隔离损坏。缓存是最容易写坏、最容易变大的一块，它挂了只丢缓存；`config` 是用户手改的，手改错了只影响设置。四个文件也意味着写放大可控——改个光标不必重写最近文件列表。

**为什么不合并 `State` 和 `Session`**：语义不同。`State` 是"编辑器的长期习惯"（MRU、面板开关），越久越有价值；`Session` 是"上一次没干完的事"，只服务于恢复现场，可以被随时清空。

---

## 4. 文件格式

自研的行式 `key = value`，不使用 JSON / TOML / serde。

```
# i-edit state v1
file_tree_visible = true
last_dir = /work/i-edit
file = /work/i-edit/src/app.rs
file = /work/i-edit/src/fs.rs
dir = /work/i-edit
```

理由：

- **零依赖。** 数据形态是"若干标量 + 若干路径列表"，引入 serde 换来的只是转义和嵌套，而嵌套这里用不上。
- **可手改、可 diff。** 状态文件是人会去看的东西。
- **容错解析。** 未知 key、坏行、缺 `=` 的行一律跳过，坏一行只丢一条；这是"读永不失败"的实现基础。

规则：

| 规则 | 说明 |
| --- | --- |
| 值写到行尾 | 只去掉 `=` 后的**一个**空格；路径里的空格、结尾空格原样保留（真正 Tail 正确） |
| 只剥 `\r` | CRLF 由 `fs::read_text_file` 处理，此处兜底；不做 `trim` 以免吞掉路径尾部空格 |
| 重复 key 合法 | 列表就是重复 key，保持文件顺序即 MRU 顺序 |
| 含换行的值拒收 | `set` / `push` 返回 `false` 并不写入。宁可丢一条路径（warn），也不写回一个读不通的值 |
| 未知 key 忽略 | 旧版本 / 新版本 / 手改残留共存 |

`cache/dirs` 不用 KV，用顺序分组（`dir` 行后跟若干 `entry` 行），因为它要表达"目录 → 子项"的层级，而 KV 表达不了：

```
# i-edit cache v1
dir 1759740000000000000 /work/i-edit/src
entry 0 /work/i-edit/src/app.rs
entry 1 /work/i-edit/src/storage
```

---

## 5. 模块结构

```
src/storage.rs            门面：Storage、Section、原子写、防抖
src/storage/paths.rs      根目录解析（I_EDIT_HOME / APPDATA / XDG）
src/storage/codec.rs      行式 KV：Document::parse / lines / get / set / push
src/storage/config.rs     Config（设置）
src/storage/state.rs      State（MRU、开关、last_dir）
src/storage/session.rs    Session（last_file、last_folder、FileView、展开目录）
src/storage/cache.rs      Cache（目录列表 + mtime 校验）
src/storage/tests.rs      门面测试
```

依赖方向单向：`storage → fs → std`。`fs` 只把 `home_dir` 放开成 `pub(crate)` 供 `paths` 复用，别无侵入。

---

## 6. API 与写入策略

```rust
let mut storage = Storage::load();          // 永不失败，失败即默认值
storage.edit_state(|s| s.touch_file(path)); // 闭包式修改，顺带打脏标记
storage.tick();                             // 主循环空闲时调用，自己做防抖
storage.flush()?;                           // 退出前调用，可能失败并上报
```

### 闭包式修改而非 `&mut`

`edit_config` / `edit_state` / `edit_session` / `edit_cache` 收 `FnOnce(&mut T)`，**不**对外暴露 `&mut Config`。暴露裸引用等于允许调用方绕过脏标记，状态改了却没落盘——这类 bug 极难发现。代价是深层修改要写闭包，可接受。

### 脏位 + 防抖

- 每个 section 一个 bit（`Section::bit()`），`edit_*` 置位并记录首个脏时间。
- `tick()` 距首次变脏超过 `FLUSH_DEBOUNCE`（800ms）才写。连按光标键 = 一次写；崩溃丢 1 秒历史。
- `flush()` 立即写所有脏 section，清位。退出路径用它。
- `tick()` 目前只在每轮事件后调用一次：主循环仍是阻塞 `event::read()`，长时间无输入时脏数据不会被写出（退出时 `flush` 兜底）。主循环 poll 化后（见 `color-rendering-plan.md` Phase 2）把它挪到超时分支即可。

### 原子写

`write_atomic`：写 `<file>.tmp` → rename 到目标。rename 在同目录内，要么拿到旧文件要么拿到新文件，不会拿到半个。Windows 不允许 rename 覆盖已存在文件，故失败分支先 `remove_file` 再 rename——这一瞬间目标不存在，但"文件缺失"正是 storage 本来就要处理的情形（`read_lines` 视 NotFound 为首次运行）。

### 写放大

只写脏的 section；`persist_cache = false` 时 cache 段直接在 `flush` 里跳过（不删已存在的缓存文件，只是不再更新）。

---

## 7. 缓存策略

**唯一缓存对象：目录列表。** 打开目录是文件树唯一真正的 I/O，启动时重读一遍是可感知的成本；其余（高亮、行宽、排序）都是进程内派生，落盘不划算。

- **校验靠 mtime，不靠 TTL。** `Cache::get(path, modified)`：调用方先 stat（它本来就要 stat），把 mtime 传进来；相等才命中。目录被改动后 mtime 变化 → 自动失效。无后台线程、无过期扫描。
- **过期与缺失同义。** 缓存从不"返回可能陈旧的数据"，不命中就重读。
- **容量 256 目录，** 超限时删一个（`HashMap` 顺序不定，不是严格 LRU；缓存是优化，淘汰不准只是多读一次）。落盘时按路径排序输出，保证文件可复现。
- **可关。** `persist_cache = false` 时只在内存里缓存，适合慢盘 / 移动盘工作区。

---

## 8. 失败与降级口径

| 情形 | 行为 |
| --- | --- |
| section 不存在 | 静默用默认值（首次运行的正常态） |
| section 读失败（权限 / 坏块） | `warn!` + 默认值，编辑器照常启动 |
| section 内容坏 | 逐行容错，坏行丢弃，其余保留 |
| 值解析失败 | 视作缺失，回退默认值（手改错了不会崩） |
| 容量超限 | 截断，不报错 |
| 后台 flush 失败 | `warn!`，脏位保留，下次 `tick` 重试；**绝不**因此退出编辑器 |
| 退出前 flush 失败 | 返回 `Err`，由 `main` 提示"状态未保存"；编辑内容本身早已由 `fs` 落盘，不丢用户文件 |
| 无法确定根目录 | 降级为内存模式，一切照常 |

核心原则：**读永不失败，写失败必须可见。** 编辑器"没记住上次打开哪个文件"是可接受的，"记住了错的"或"打不开"是不可接受的。

---

## 9. 接入点（现状 → storage）

**Phase 0–2 已接线完成**，下表是最终形态，供回看与审阅：

| 位置 | 现在 |
| --- | --- |
| `main.rs` | `App::new(Storage::load())` → `ratatui::run(...)` → `app.persist()`；`ran.and(persisted)`，运行错误优先上报，持久化失败只在没有别的错误时上报 |
| `App` | 持 `storage: Storage`；`App::default()` = `Storage::default()`（**无根、纯内存**，测试用它，不碰真实存储）；另有 `expansion_buf` 复用容量 |
| `load_file_now` | 先 `remember_current_view()`（记住被替换的 buffer），再 `session.last_file` + `state.touch_file` + `state.last_dir` |
| `write_buffer` | `state.touch_file` + `state.last_dir` |
| `open_folder` | `session.set_folder`（顺带清展开集合）+ `state.touch_dir` + `state.last_dir` |
| `toggle_file_tree` | `state.file_tree_visible` |
| `picker_start_dir` | 该 picker 上次目录（须仍存在）→ 文件树 root → `state.last_dir`（须仍存在）→ cwd（cwd 是进程态，不入库） |
| `load_file_now` / `open_folder` / `save_to` / 覆盖确认 | 顺带 `state.set_picker_dir(kind, dir)`，让对应的弹窗下次从这里开始 |
| 主循环每轮 | `apply_expansion_changes()` 把树的展开变更搬进 session，然后 `storage.tick()` |
| 退出前（含 `Quit`） | `remember_current_view()`，然后 `main` 里 `persist()` |
| `restore_session` | `file_tree_visible` 先恢复（不受 `restore_session` 开关影响）；再 `last_folder` → `restore_folder`（含展开，上限 32 个目录）；再 `last_file` → `restore_file`（光标 + `scroll_y`，`clamp_cursor` 兜底） |

树的展开变更走 `FileTreeState::take_expansion_changes_into`（`Vec<(PathBuf, bool)>` outbox，与 `Actions` 同构）：组件不能碰 storage，只能上报，由 shell 落库。

命名提醒：`FileTreeState::set_expanded` 与 `Session::set_expanded` 同名不同义，前者是树自己的展开动作（会读盘），后者是把该动作记进会话。

---

## 10. 落地阶段

| Phase | 内容 | 状态 |
| --- | --- | --- |
| 0 | `paths` / `codec` / 四个 section / `Storage` 门面 / 原子写 / 防抖 / 测试 | ✅ 已落地 |
| 1 | `App` 持有 `Storage`；替换 `last_dir` 与 `file_tree_visible`；`main` 启动加载 + 退出 flush | ✅ 已落地 |
| 2 | 会话恢复：`last_file` + 光标 + `scroll_y`、`last_folder` + 树展开；受 `restore_session` 控制 | ✅ 已落地 |
| 3 | 缓存接入：文件树 children 走 `storage.cache`（mtime 校验）；picker 起始目录用 `recent_dirs` | 待做 |
| 4 | MRU 的 UI：命令面板 / picker 展示最近文件；容量清理命令；`i-edit.log` 迁到存储根目录 | 待做 |

Phase 0 刻意不接 UI：四段数据模型与"读永不失败"的口径先定死，接线只是替换字段来源。

**测试现状**：`storage` 19 个、`app` 26 个。重启恢复 5 例（文件+光标、光标越界 clamp、文件夹、面板开关、文件已删除）；picker 起始目录 4 例（三个弹窗各记各的、跨重启生效、目录已删除时回退、另存回到写入目录）。`App::default()` 用无根 storage，测试绝不会写到用户的真实存储目录。

---

## 11. 零分配口径

storage 全在冷路径（启动、空闲 tick、退出），**不受 `allocation-plan.md` 的每帧约束**。但沿用项目的做法：

- 写复用 `fs::write_text_file` 的一次精确分配；
- 读复用 `fs::read_text_file` 的逐行 `read_until`（不整文件读入）；
- 无 `Box` / `Rc` / `Arc` / 全局状态；`HashMap` 只出现在 `cache`（冷路径）；
- 不引入运行时 / 后台线程，无锁。

`tick()` 每轮被主循环调用，内部是脏位与 `Instant` 比较，**无任何分配**。

---

## 12. 风险与未决项

| 项 | 现状 / 决定 |
| --- | --- |
| 多实例并发写 | v1 不处理（无锁文件）。同时开两个 i-edit 会互相覆盖 state，后退出者胜。需要时加 `<root>/lock` 存 pid |
| 路径规范化 | 存绝对路径；符号链接、大小写差异未规范化，可能存出两条指向同一文件的 MRU。需要时对存在的路径做 `canonicalize`（失败则原样存） |
| 路径含换行 | `codec` 拒收，该条不落盘（warn）。Unix 上理论可行，实际不构成问题 |
| 隐私 | 最近文件列表是明文路径。个人用途可接受，不额外加密 |
| 缓存无限增长 | 有 256 硬顶；`MaxViews` 200 / `MaxExpandedDirs` 500 同理。无按时间的老化策略，靠淘汰 |
| Windows rename 覆盖 | 已处理（先 remove 再 rename） |
| 日志位置 | 现在写 CWD 的 `i-edit.log`，Phase 4 迁到存储根目录 |
| 版本迁移 | 文件头带 `v1`；目前解析忽略未知 key，即"前向兼容"靠忽略，真要改语义时再写迁移 |

---

## 13. 测试

`src/storage/tests.rs` 覆盖门面，`codec` / `paths` / `cache` 各自带单元测试。所有测试用自删临时目录作 root，**不碰真实存储**。

已覆盖：空 root 默认值、跨重启恢复、section 独立写、坏文件隔离、重复 flush 覆盖、MRU 上限与置顶、缓存 mtime 命中/失效、缓存上限、`persist_cache` 关闭、无 root 降级。

接入 UI 后补充：启动恢复光标的 clamp（文件变短/变长）、`canonicalize` 行为、退出路径（`run` 返回 `Err` 时也要 flush）。
