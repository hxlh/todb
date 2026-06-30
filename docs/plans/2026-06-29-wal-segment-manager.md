# Plan: 收敛 WAL 段状态管理到 SegmentManager（含正确读写流程）

**Date**: 2026-06-29
**Type**: Architecture change (WAL internals — state layer + read/write flow)
**Status**: Revised post-audit (4 blocking issues addressed); recovery deferred. 把 segment 状态从散落在 `Inner.segments`（`SegmentState { sealed, active_seg, active_mem }`）+ `FlushState { cur_seg, cur_mem, seg_written, seg_min/max_lsn }` 收敛到**三层**：`Wal`（门面）+ `BufferManager`（原 `FlushState`，buffer 池生命周期 + flush 线程循环）+ `SegmentManager`（段状态 + 段 IO）。本 plan 实现**正确的读写流程**：`append` 落盘 + rollover 产生 sealed + `scan`/`get` 读 sealed（`.idx` footer → sst iter）/ active（memtable iter）+ `truncate`。**Recovery（重启从磁盘重建）保留位置不实现** —— `SegmentManager::open` 本 plan 只建全新 WAL（与当前 `Wal::open` 行为一致，`impl.rs:75-76` 明示不 recover），recover 留 TODO 钩子，待读写流程验证后另做。IO 装备优化（DiskManager 稳定实例、`.idx`/`.log` cache 隔离、lazy-open/ulimit）亦留后续 plan。
**Autonomy**: plan-first (protected area: WAL internals)
**Reviewer**: subagent

## Goal

让 WAL 的段状态有单一真理来源、单一行为入口，并把读写流程跑通正确。`SegmentManager` 成为段的唯一 owner：flush 线程（`BufferManager::run`）只调 `append` 落盘，Wal 对外只调 `scan`/`get`/`truncate`；段的创建/seal/定位/切换全部内聚到 Manager。

## Audit-Driven Revisions (vs 初版)

初版经 architect subagent 审计为 no-go，4 个 blocking 已解：

1. **[原 #1] sealed 读路径**：从"搬运 `from_sealed_segment` 的 `meta_fd` 读 `.idx` bug"改为**正确实现** —— `SegIndex::Sealed { idx_fd, footer }`，rollover 时开 idx_fd + decode footer 填充，scan 用 footer 构建 sst iter（不重 decode）。sealed 段 scan 端到端可验，不再是 expected-fail。
2. **[原 #2] durable_lsn 推进**：`append` 步骤补全为 `pwrite → fdatasync → mem.put → 返回 max_lsn`；`BufferManager::run` 在 append 返回后推进 `durable_lsn`（Wal 层 `Arc<AtomicU64>`）。新增不变量：durable_lsn 推进晚于 fdatasync 晚于 mem.put。
3. **[原 #3] 迁移真理归属**：改用**三层分离**，段状态零双轨 —— `FlushState` 段字段（`cur_seg`/`cur_mem`/`seg_min/max_lsn`）全删归 Manager；`FlushState` 收窄为 `BufferManager`（只管 buffer）。Phase 切换 = flush 线程从操作 FlushState 字段改为调 `manager.append`，一步切，无双轨窗口。
4. **[原 #4] WalIter durable_lsn 过滤**：`WalIter` 新增 `durable_lsn: Lsn` 字段，`build_iter` 从 Wal 快照传入，`value()`/`next()` 过滤 `key > durable_lsn`。当前 `WalIter::new(_range)` 的 `_range` 未使用（`iter.rs:200`），本 plan 接上。

非 blocking：Risk #1 论证改写（rollover 正确性真正保障 = finalize 不清空 mem + `.log` 只追加 + entry 不可变，**不是**"`.idx` durable 先于 `.meta`"）；测试补强（rollover 中并发 scan、truncate unlink 死段中 scan、durable_lsn 边界）。

## Settled Decisions

1. **三层分离** —— `Wal`（门面：`append` 无锁写 buffer + `durable_lsn` + `scan`/`get`/`truncate` 委托）/ `BufferManager`（buffer 池 + flush 线程循环）/ `SegmentManager`（段状态 + 段 IO）。buffer（内存缓冲生命周期）与 segment（落盘数据组织）是正交关注点，分开。
2. **`SegEntry` 不可变** —— 状态变更 = 锁内构造新 entry 替换 map 旧值，或 dead 段移出。`Arc<SegEntry>` 可被 scan 长期持有。
3. **IO 锁外、状态指针切换锁内** —— `seal_and_roll`/`truncate` 的落盘在 `RwLock` 临界区外，锁内只换指针。
4. **get 复用 scan 的 `build_iter`** —— `get(lsn)` = `build_iter(点 range)` + `seek(lsn)` + `key == lsn` 判等。消除 `locate`。
5. **`MemTable` 已并发安全** —— `crossbeam_skiplist::SkipMap`（`memtable.rs:61`）。active 段 memtable 由 flush `put` / scan `iter()` 直接并发共享（方案 A，无需 snapshot）。
6. **sealed 正确读** —— `SegIndex::Sealed { idx_fd, footer }`：rollover 时 `finalize` 写完 `.idx` 后开 idx_fd + block-aligned tail read decode footer，填进 SegEntry；scan 用 footer 构建 sst iter（footer 不重 decode）。`idx_fd` 进 `SegEntry::Sealed`（sealed 专属），`Segment` 仍只持 `log_fd`/`meta_fd`。
7. **`durable_lsn` 是 Wal 层 `Arc<AtomicU64>`** —— `BufferManager::run` 在 `manager.append` 返回 max_lsn 后推进；scan 读它快照传 `WalIter` 作可见性边界。
8. **Recovery 延后** —— `SegmentManager::open` 本 plan 只建全新 seg0；重启 recover（discover `seg_*.meta` + sealed/active 判定 + active `.log` tail replay + `.meta` 双写恢复）留 TODO 钩子，待读写流程验证后另做。
9. **`BufferManager` 收纳 buffer 管理** —— `active`/`free_pool`/`flush_tx`/`swap_lock`/`swap_cv` 从 `Inner` 迁入 `BufferManager`；`try_swap_full`/`wait_active_change`/`recycle_buffer` 随之。`Wal::append` 的"取 active + swap"经 BufferManager，"抢槽 + encode"在 `WalBuffer`。

## Design

### 三层架构

```
Wal (门面, 对外)
  ├ append(payload)    → BufferManager 取 active → WalBuffer fetch_add 抢槽 → 满了 BufferManager::swap
  ├ durable_lsn()      → 读 Arc<AtomicU64>
  ├ scan/get/truncate  → 委托 SegmentManager
  └ 持 Arc<BufferManager> + Arc<SegmentManager> + Arc<AtomicU64>(durable_lsn)

BufferManager (原 FlushState, buffer 池 + flush 线程)
  ├ 持 active / free_pool / flush_tx / swap_lock / swap_cv
  ├ try_swap_full / wait_active_change / recycle_buffer
  └ run(rx): drain buf → SegmentManager::append(buf, written) → 推进 durable_lsn → recycle
              (written 是循环局部变量; 关闭时调 manager.seal_active())

SegmentManager (段唯一 owner)
  ├ 持 RwLock<Registry> + dir + config + next_seg_id
  ├ append / seal_active / scan / get / truncate_before / truncate_after (公开)
  └ build_iter / seal_and_roll (私有)
```

### 数据结构

```rust
enum SegIndex {
    Active(Arc<MemTable<Bytes, Bytes>>),           // flush put / scan iter 并发共享
    Sealed { idx_fd: RawFd, footer: SstFooter },   // rollover 填充；scan 用 footer 构建 sst iter
}

struct SegEntry {
    seg: Arc<Segment>,     // log_fd + meta_fd + seg_id（I/O 句柄，不变）
    meta: IdxHeader,       // seg_id / min_live_lsn / max_live_lsn / entry_count —— 内存副本
    index: SegIndex,       // 角色隐含在变体里
}

struct Registry {
    by_id: BTreeMap<u32, Arc<SegEntry>>,   // 全段，seg_id 有序；scan 按 meta lsn 范围相交过滤
    active_id: u32,
}

struct SegmentManager {
    registry: RwLock<Registry>,
    dir: PathBuf,
    config: WalConfig,
    next_seg_id: AtomicU32,
}
```

写游标（active 段已写字节）是 flush 线程局部变量（`BufferManager::run` 的 `written`），`append(buf, written) -> (new_written, max_lsn)`，不进 Manager（高频变化，避免进 `RwLock<Registry>` 阻塞 scan）。

### 接口

**原则**：只迁移现有方法，不自造访问器。buffer 字段 `pub(crate)` 直接访问（同当前 `inner.active`/`inner.free_pool`）；段状态经 Manager 方法。

```rust
// Wal（pub，对外 API）
impl Wal {
    pub fn open(dir, config) -> Result<Self, WalError>;            // 建三组件 + spawn flush
    pub fn append(&self, payload: &[u8]) -> Result<Lsn, WalError>; // buf_mgr.active.load_full() 抢槽 + swap（encode 在 WalBuffer）
    pub fn durable_lsn(&self) -> Lsn;                               // 读 Arc<AtomicU64>
    pub fn sync(&self, lsn, deadline) -> Result<(), WalError>;      // 现有轮询逻辑
    pub fn scan/get/truncate_before/truncate_after;                 // 委托 seg_mgr
    pub fn close(&self) -> Result<(), WalError>;                    // stop + join + seg_mgr.seal_active
}

// BufferManager（pub(crate)；方法全迁移自现有，无自造访问器）
impl BufferManager {
    fn new(config, seg_mgr: Arc<SegmentManager>, durable_lsn: Arc<AtomicU64>) -> (Arc<Self>, Receiver<Arc<WalBuffer>>);
    fn run(self: Arc<Self>, rx: Receiver<Arc<WalBuffer>>);  // drain → seg_mgr.append → durable_lsn.store → recycle；written 局部
    fn try_swap_full(&self, old: &Arc<WalBuffer>);          // 迁自 impl.rs:252
    fn wait_active_change(&self, old: &Arc<WalBuffer>);     // 迁自 impl.rs:302
    fn recycle_buffer(&self, buf: &Arc<WalBuffer>);         // 迁自 impl.rs:524
    fn shutdown(&self);                                      // stop_flag + join（close 用）
}
// active / free_pool / flush_tx / swap_lock / swap_cv / stop_flag 字段 pub(crate)，Wal::append 直接访问

// SegmentManager（pub(crate)）
impl SegmentManager {
    fn open(dir, config) -> Result<Self, WalError>;         // 建全新 seg0；recover 分支 todo!
    fn append(&self, buf, written: u64) -> Result<(u64, Lsn), WalError>;  // (new_written, max_lsn)
    fn seal_active(&self) -> Result<(), WalError>;
    fn scan/get/truncate_before/truncate_after;
    // 私有
    fn active(&self) -> Arc<SegEntry>;                      // 持读锁取写目标段（append/seal_and_roll 内部）
    fn build_iter(&self, range) -> Result<WalIter, WalError>;
    fn seal_and_roll(&self, new_min_lsn: u64) -> Result<(), WalError>;
}
```

`scan` = `build_iter(range)`；`get` = `build_iter(点 range)` + `seek(lsn)` + `key == lsn` 判等。

> **注**：`BufferManager` 无 `active()` 访问器方法——`Wal::append` 直接 `self.buf_mgr.active.load_full()`（字段 `pub(crate)`），与当前 `self.inner.active` 访问方式一致。`SegmentManager::active()`（私有）是另一回事：返回 `Arc<SegEntry>`，供 append/seal_and_roll 取写目标段，非公开访问器。

### 不变量（4 条）

1. **entry 不可变** —— 状态变更 = 锁内换 entry / 移出 dead 段。
2. **IO 锁外、状态锁内** —— 落盘在 `RwLock` 临界区外，锁内只换指针。dead 段 `unlink` 三文件（Linux 下持 fd 的 scan 仍可读到结束）。
3. **durable_lsn 推进时序** —— `durable_lsn` 推进晚于 `.log` fdatasync 晚于 `mem.put`（推进时 frame 既 `.log` durable 又 mem 可见）。`WalIter` 用 durable_lsn 过滤 `key > durable_lsn`。
4. **rollover 并发正确性** —— 保障是"finalize 不清空 memtable + `.log` 只追加不覆盖 + entry 不可变"，**不是**"`.idx` durable 先于 `.meta`"（后者只管崩溃恢复）。finalize 写 `.meta` 期间，scan 经 entry 不可变对 `.meta` 改写免疫（scan 持的 `Active(mem)` 快照不读 `.meta`）。

## Scope

### In Scope

- **P1 数据结构**：`SegIndex` / `SegEntry` / `Registry` / `SegmentManager` / `BufferManager` 类型（`wal/manager.rs` 新文件）。
- **P2 Manager 骨架**：`SegmentManager::open`（建全新 seg0 + 初始 Registry；recover 分支留 `todo!` + TODO 注释）、6 接口签名 + 私有签名。
- **P3 BufferManager**：从 `Inner` 迁入 buffer 字段 + `try_swap_full`/`wait_active_change`/`recycle_buffer`；`run` 循环（drain → `manager.append` → 推进 durable_lsn → recycle）；`Wal::append` 的取 active/swap 改委托 BufferManager。
- **P4 append + seal_and_roll**：`append` = rollover 判断 → pwrite `.log` → fdatasync → mem.put → 返回 `(new_written, max_lsn)`；`seal_and_roll` = 锁外 finalize（drain mem→`.idx` + 写 `.meta`）→ 锁内换 entry（旧 active → `Sealed{idx_fd,footer}` + 新 `Active(new_mem)`）。
- **P5 build_iter + scan + get**：持读锁 clone 相交 entries → drop 锁 → sealed 用 `{idx_fd, footer}` 构建 sst iter、active 用 mem iter → `TwoMergeIter` 合并 + meta live-range 裁 truncate；`WalIter` 加 `durable_lsn` 字段过滤；`get` 复用 + seek 判等。**过滤点语义（再审条件 1）**：`next()` 须 **skip** `key > durable_lsn` 的 entry（不 yield、`key()`/`valid()` 不暴露该 lsn），`value()` 作兜底——避免 crossbeam iter 非确定视图让未 fsync 的 lsn 泄漏给 `TwoMergeIter` 污染合并排序。测试场景 #8 兜底验证。
- **P6 truncate**：`truncate_before/after` 改各段 meta 内存副本 + 落盘 `.meta`（锁外）+ dead 段移出 + unlink。
- **P7 收尾**：删 `SegmentState` / `FlushState` 段字段 / `Inner.segments`；`Wal` 持 `Arc<BufferManager>` + `Arc<SegmentManager>` + `Arc<AtomicU64>`；`scan/get/truncate_*` 委托。

### Out of Scope

- **Recovery**（重启从磁盘重建）：discover `seg_*.meta` + sealed/active 判定 + active `.log` tail replay + `.meta` 双写恢复 + crash mid-seal 处理。本 plan 留 TODO 位置，后续 plan 做。
- **IO 装备优化**：DiskManager 稳定实例（替代 scan 现场 `Arc::new`）、`.idx`/`.log` cache 隔离（`BlockKey(seg_id,block_idx)` 不含文件类型）、cache 命中验证（`PinGuard::as_ptr`）、idx_fd lazy-open/ulimit。后续 plan。
- `MemTable` 改造（已并发安全）；`Wal::append` 无锁热路径（不动）。

## Implementation Strategy (Skill: none for all phases)

- **P1**：`wal/manager.rs` 定义全部类型。`SegIndex::Sealed` 实含 `{idx_fd, footer}`（非占位）。
- **P2**：`SegmentManager::open` 建 seg0（`entry_count=0` active）+ Registry；recover 分支 `todo!("recovery deferred — see plan Open Questions")`。
- **P3**：`BufferManager` 持 buffer 五字段；`run` 循环 `written` 局部变量；durable_lsn 推进点在 append 返回后。
- **P4**：`append` 严格按 pwrite→fdatasync→mem.put→返回 顺序（对齐 `impl.rs:413-433` 现有顺序）；`seal_and_roll` 锁外 finalize 后开 idx_fd + decode footer（block-aligned tail read，复用 `disk.rs::raw_read` + `from_sealed_segment` 的 footer 解析逻辑但**用 idx_fd 非 meta_fd**），锁内换 entry。
- **P5**：`build_iter` sealed 分支用 `SegEntry::Sealed` 已有的 `{idx_fd, footer}` 构建 `WalIndexReader` + `SstIter`（不重 decode footer）；`WalIter` 加 `durable_lsn` 字段，`value()`/`next()` 过滤。
- **P6**：truncate 锁外算新 meta + 落盘，锁内换 entry / 移 dead 段 + unlink。
- **P7**：一步删旧字段，Wal 委托 Manager。

## File Changes

| File | Change |
|------|--------|
| `wal/manager.rs` (new) | `SegIndex`/`SegEntry`/`Registry`/`SegmentManager`/`BufferManager` + 接口 + `build_iter`/`seal_and_roll` |
| `wal/impl.rs` | `Wal` 持 `Arc<BufferManager>`+`Arc<SegmentManager>`+`Arc<AtomicU64>`；`append` 委托 BufferManager；`scan/get/truncate` 委托 Manager；删 `Inner.segments`/`SegmentState`/`FlushState` 段字段；`BufferManager::run` 取代 `FlushState::run` |
| `wal/iter.rs` | `WalIter` 加 `durable_lsn` 字段 + 过滤；构建逻辑迁入 `SegmentManager::build_iter`（`from_sealed_segment` 改用 `SegEntry::Sealed` 的 idx_fd/footer） |
| `wal/mod.rs` | `pub mod manager` + 导出 `SegmentManager`/`BufferManager` |
| `wal/disk.rs` | `raw_read` 保留（footer decode 用）；不改 IO |
| `wal/segment.rs` | 不改（`log_fd`/`meta_fd` 不变；`idx_fd` 归 `SegEntry::Sealed`） |

**Estimated impact**: ~5 文件，~700–1000 行（含测试）。

## Risks

1. **rollover 锁序** —— 见不变量 #4：真正保障是 finalize 不清空 mem + `.log` 不覆盖 + entry 不可变。finalize 写 `.meta` 期间 scan 经 entry 不可变免疫。缓解：commit 不变量（`.idx` durable 先于 `.meta`）保证 sealed entry 的 `.idx` 可读（崩溃恢复视角）。
2. **truncate 与 scan 并发** —— dead 段 unlink 后持 fd 的 scan 仍可读完（Linux unlink 语义）；entry 从 map 移出但 Arc 引用计数兜底。
3. **append 单线程约束** —— 只 `BufferManager::run` 调，靠文档约定（非类型保证）。
4. **durable_lsn 边界** —— `WalIter` 必须主动过滤 `key > durable_lsn`（crossbeam iter 视图非确定性，靠此边界收敛）。
5. **idx_fd 生命周期** —— `seal_and_roll` 时开（finalize `.idx` 后）、段 drop 时关；SegmentManager 管。lazy-open/ulimit 留后续。

## Verification

```bash
cargo check -p storage
cargo test -p storage --lib
cargo test -p storage wal
cargo clippy -p storage -- -D warnings
```

**Test scenarios**（`src/tests/wal_segment_manager.rs` 或并入现有）：
1. `append` 单段：N buffer → active `.log` 字节 + active mem index 正确（lsn→offset,len 与 frame 交叉校验）。
2. `append` rollover：跨 `segment_size` → 旧段 seal（`.idx` + `.meta` entry_count>0 + `SegEntry::Sealed{idx_fd,footer}` 填充）、新 active 建立；Registry 两段、active_id 正确。
3. `scan` 合并 sealed + active：sealed sst iter（用 footer）+ active mem iter 合并，按 lsn 升序、无重复无遗漏（**本 plan 核心验证：sealed 读路径正确**）。
4. `get` 命中/未命中：存在 lsn 返回 record；不存在（含 truncated）返回 None（seek 判等）。
5. `get` 复用 scan：同范围结果一致。
6. `truncate_before/after`：改 `.meta`、dead 段移出 + unlink；scan 不返回 truncated lsn。
7a. **rollover 中并发 scan**：rollover 进行中触发 scan，断言返回 lsn 序列连续无缺无重复（不变量 #4 验证）。
7b. **truncate unlink 死段时 scan 读完**：scan 持 dead 段 entry → truncate unlink → 断言 scan 读完该段（Linux unlink 语义验证）。
8. **durable_lsn 边界**：写 N frame 不推进 durable_lsn → scan 返回数受边界约束（不变量 #3 验证）。

**Smoke checklist**:
- [ ] `FlushState` 段字段删除；`BufferManager` 只持 buffer 五字段 + run 循环。
- [ ] `Inner.segments`/`SegmentState` 删除。
- [ ] `Wal::scan/get/truncate_*` 全部委托 Manager。
- [ ] sealed 段 scan 用 `SegEntry::Sealed` 的 idx_fd/footer（不再用 meta_fd 读 `.idx`）。
- [ ] `WalIter` 有 `durable_lsn` 字段且过滤生效。
- [ ] `SegmentManager::open` 有 recover TODO 钩子（未实现，位置保留）。

## Closure Criteria

1. `SegmentState` / `FlushState` 段字段删除；段状态只在 `SegmentManager::Registry`。
2. 三层分离落地：`Wal` 门面 + `BufferManager`（buffer）+ `SegmentManager`（段）。
3. `append/scan/get/truncate_before/truncate_after/seal_active` 走 Manager；读写流程端到端正确（含 sealed 段 scan）。
4. `cargo build --workspace` clean；`cargo clippy -p storage -- -D warnings` 无警告。
5. 现有 WAL 测试全绿 + 新增场景 1–8 通过。**测试须在空目录跑**（再审条件 2：recovery 未实现，既有 `.meta` 无对应 `.idx` 会让 sealed 读路径 panic；recover 留 TODO，见 Closure #7）。
6. `Wal`/`BufferManager` 不再直接操作段生命周期。
7. Recovery 位置保留（`SegmentManager::open` recover 分支 `todo!` + 注释），未实现。
8. IO 行为零改变（除 sealed 读 bug 修复：idx_fd 取代 meta_fd）—— DiskManager/cache/fd 其余不动。

## Dependencies

- **Requires** (done): wal-index-read-path Phase 1/2 —— `WalIter` 草稿、`raw_read`、`finalize_segment`、`ODirectSstWriter` 是素材。
- **Blocks**: recovery plan（重启重建）、IO 装备 plan（DiskManager 稳定实例 + cache 隔离 + lazy-open）；wal-index-read-path Phase 4 truncate（本 plan 提供状态层地基）。

## Open Questions

- **Recovery plan 时机**：读写流程验证通过后立 recovery plan，还是与 IO 装备 plan 合并？倾向先 recovery（正确性），再 IO 装备（性能）。
- **durable_lsn 推进机制**：`append` 返回 max_lsn 由 `BufferManager::run` 推进（本 plan 方案），还是 append 接收 `&AtomicU64` 直接推进？倾向前者（Manager 不持 durable_lsn，职责纯）。
- **`append` 入参**：`&WalBuffer`（耦合 buffer 布局）还是 `&[FrameMeta]`？倾向 `&WalBuffer`（与现有 `flush_buffer` 一致，减少改动面）。
- **idx_fd 获取**：`seal_and_roll` finalize 写 `.idx` 后重开 idx_fd 读（写读两次 open），还是 finalize 保留 fd 转读？倾向重开（`ODirectSstWriter` 设计写完关，简单）。
- **`WalIter` 归属**：留 `wal/iter.rs` 还是并入 `manager.rs`？倾向留 `iter.rs`（类型 + frame 读取），`build_iter` 在 manager 组装。

## Notes

- `docs/plans/00-plan-authoring-and-execution-guide.md` 被 AGENTS.md/CLAUDE.md 引用为 controlling workflow，但在 `docs/plans/`、`docs/archive/`、全 `docs/` 下均不存在（已按 AGENTS.md 规则 13 检查 archive）。本 plan 格式参照同目录 `2026-06-27-wal-index-read-path.md`。guide 缺失需用户决定补建或豁免。
- 本 plan 经一轮 architect subagent 审计（no-go → 4 blocking → 修订），修订版建议进实现前可再审一轮确认 blocking 已闭合。
