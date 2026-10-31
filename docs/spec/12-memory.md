# Memory and allocation

This chapter defines allocators and the rule that allocation is authority, budgets with their cells and chunks, generational handles, growth and its two failure forms, the stores `mem::Heap`, `mem::Arena[T]` and `mem::Pool[T, B]` with their ids, handles, boxes and regions, chunk ownership, `mem::Shared[T]`, the collections and their eager methods, interior mutability with `mem::OnceMap[K, V]`, and the properties of these types.

## Allocators

An **allocator** is a value of `mem::Alloc`, the [capability](14-authority.md#capabilities) to draw memory. Apart from the memory of the [support routines](27-program-model.md#runtime-support), a program draws memory at run time only through an allocator, held as a value or stored by a container or store. No global allocator exists. No item of `core` draws memory itself: a `core` item's [digest](10-effects-digests.md#digests) has the alloc fact only through a call of a `mem` item, such as those the content-blind operations on `Untrusted[mem::Bytes]` make ([content-blind operations](15-untrusted.md#content-blind-operations)), or through a bound or a function value.

- `mem::Alloc` is a Copy primitive capability and a [generational handle](#generational-handles) naming a [cell](#budgets): the root cell of a root grant or of a store over a region, the cell of the budget that returned it, or the cell of a revocable allocator. Every copy names the same cell.
- An allocator is stale once the cell it names is closed. No allocator operation fails with `sys::Revoked`: a draw through a stale allocator is exhaustion ([budgets](#budgets)).

```nan-api
// module: mem

pub open struct Alloc {
} Alloc

pub fn Alloc.split(self, limit: usize) -> Budget
```

```nan
pub fn readings_list(values: Slice[u16], alloc: mem::Alloc) -> mem::List[u16] {
    var readings = mem::List[u16].empty(alloc)
    for value in values {
        mut readings.push(value)
    }
    return readings
} readings_list
```

## Budgets

A **budget** is a linear `mem::Budget` owning a cell. A **cell** is a slot of a cell table holding a limit in bytes, a charge in bytes, a parent cell and an open or closed state; cell tables are memory of the support routines, charged to no cell. A **chunk** is 64 KiB of memory aligned to 64 KiB; cells are charged in whole chunks.

- A **root cell** is a cell without a parent; its **supply** is the memory its chunks come from. The allocator of a [root grant](14-authority.md#root-grants) names a root cell with a limit of 2^64 − 1 bytes, whose supply is the process's memory on the host targets and the boot heap ([anchor](24-kernel.md#anchor)) on the machine targets. A store over a region names a root cell whose supply is that region ([regions](#regions)).
- Every other cell is a budget's or a revoker's, and its parent is the cell of the allocator it was made from. The **chain** of a cell is the cell, its parent, the parent's parent and so on to a root cell.

```nan-api
// module: mem

pub open linear struct Budget {
} Budget

pub fn Budget.alloc(self) -> Alloc

pub fn Budget.limit(self) -> usize limits pure, notrap, terminates

pub fn Budget.charged(self) -> usize

pub fn Budget.close(take self)
```

- `alloc.split(limit)` adds an open cell with that limit and a charge of 0, whose parent is the cell `alloc` names, and returns the budget owning it. `split` and the members of `mem::Budget` terminate and have no effect fact. `split` traps when the support routines' memory for cell records is exhausted (`trap.exhausted`), its only trap site; the members of `mem::Budget` contain none.
- `sys::revocable(alloc)` ([revocation](14-authority.md#revocation)) adds an open cell with limit 2^64 − 1 and charge 0, whose parent is the cell `alloc` names and whose owner is the returned `sys::Revoker`, and never fails. Its `capability()` is the allocator naming that cell, and `take revoker.revoke()` closes the cell as `close` does.
- `budget.alloc()` returns the allocator naming the budget's cell; every call returns the same handle. `limit()` is the cell's limit.
- A **draw** of n chunks through an allocator succeeds exactly when every cell on the chain of the cell it names is open and has a charge that, with n × 65,536 bytes added, is at most its limit, and the root cell's supply yields n chunks. It then adds n × 65,536 bytes to the charge of every cell on that chain. Otherwise it charges nothing; a failed draw is **exhaustion**.
- Draws, returns, closes and the passing of charges ([domain memory](11-errors-domains.md#domain-memory)) are atomic: concurrent draws and passings through cells sharing an ancestor never take that ancestor past its limit, and a draw or passing either completes before the close of a cell on its chain or fails.
- A return has no [digest fact](10-effects-digests.md#digests), so code under `noalloc` may release values. On the machine targets, each draw and return runs with maskable interrupts disabled on its CPU.
- `take budget.close()` closes the budget's cell, making its allocator stale, so no draw succeeds through an allocator whose cell's chain holds that cell. Memory already drawn stays usable.
- A chunk **returns** to its root cell's supply once no allocation in it is live, or when its domain is freed ([chunk ownership](#chunk-ownership)). A return subtracts 65,536 bytes from the charge of every open cell on the chain of the cell it is charged to: the cell it was drawn through, until [domain memory](11-errors-domains.md#domain-memory) passes its charge to another.
- `charged()` is the cell's charge: 65,536 bytes for each chunk not yet returned that is charged to the cell or to a cell whose chain holds it.
- An **allocation** is the memory that one draw of a std operation obtains for one purpose: a box, the storage of a container at one capacity, the slot storage of a store, or the contents of a `mem::Shared`. It is live until the operation that releases or replaces it frees it: the release of its box, container, store or shared value, or growth moving the elements into a new allocation.
- An allocation of at most 64 KiB lies inside one chunk, which other allocations may share. A larger one occupies ⌈size ÷ 64 KiB⌉ consecutive chunks drawn together for it alone. Which chunk an allocation of at most 64 KiB lies in, when a draw happens for one, and how much memory a container or store draws for its contents beyond the capacities [growth](#growth) fixes, are the implementation's choice.
- Each choice of this chapter left to the implementation is identical for one [toolchain hash](36-builds.md#toolchain-hash), so `charged()` and the points of exhaustion are reproducible.

```nan
pub fn count_stored(values: Slice[u32], alloc: mem::Alloc) -> usize {
    let budget = alloc.split(65_536)
    var kept = mem::List[u32].empty(budget.alloc())
    for value in values {
        let Ok(_) = attempt mut kept.try_push(value) else {
            break
        }
    }
    let count = kept.len()
    take budget.close()
    return count
} count_stored

fn touch(alloc: mem::Alloc) -> usize {
    var scratch = mem::List[u8].empty(alloc)
    mut scratch.push(7)
    return scratch.len()
} touch

pub fn charge_after_release(alloc: mem::Alloc) -> usize
    example charge_after_release(testing::world().alloc) == 0
{
    let budget = alloc.split(65_536)
    discard touch(budget.alloc())
    let charge = budget.charged()
    take budget.close()
    return charge
} charge_after_release
```

```nan
pub fn scratch_size(alloc: mem::Alloc) -> usize {
    let budget = alloc.split(65_536)
    let scratch = mem::List[u8].empty(budget.alloc())
    return scratch.len() // error: own.linear-dropped
} scratch_size
```

## Generational handles

A **generational handle** is a Copy value naming one slot of a table by the slot's index and by the generation the slot had when the handle was made, each a `u32`. Cell tables, [capability tables](14-authority.md#capability-tables) and [pools](#pools) are such tables.

- A slot's generation starts at 0. Ending the slot's occupant (closing a cell, revoking a capability, removing a pool entry) adds 1 to it, which makes every handle carrying the old generation **stale**. A handle is **live** while its generation equals its slot's.
- A slot whose generation reaches 2^32 − 1 is retired and never holds an occupant again, so a stale handle never becomes live.
- The section defining each handle type states what an operation does with a stale handle.

```nan
pub closed struct Job derives Copy {
    pub id: u32
} Job

struct JobSlots {
} JobSlots

pub fn first_handle_live(alloc: mem::Alloc) -> bool
    example not first_handle_live(testing::world().alloc)
{
    var jobs = mem::Pool[Job, JobSlots].empty(alloc)
    let earlier = mut jobs.insert(Job(id: 1))
    discard mut jobs.remove(earlier)
    discard mut jobs.insert(Job(id: 2))
    return jobs.contains(earlier)
} first_handle_live
```

## Growth

A **growable container** is a container that stores the allocator it was created with and draws memory through it as it grows: `mem::List`, `mem::Bytes`, `mem::Map`, `mem::HashMap`, `mem::Set`, `mem::OnceMap`, `mem::Arena`, `mem::Pool`, and `text::String` and `text::Interner` ([text types](13-text-encode.md#text-types)). Creating one with `empty` or `over` draws no memory.

```nan-api
// module: mem

pub closed enum OutOfMemory derives Copy, Eq, Hash, Render {
    Exhausted
} OutOfMemory
```

- A std operation that may draw memory traps on exhaustion (`trap.exhausted`) unless its name starts with `try_`. Each such operation of this chapter other than `make_mut` has a `try_` form (`push` and `try_push`, `boxed` and `try_boxed`), which fails with `mem::OutOfMemory.Exhausted` instead, leaving its receiver unchanged and releasing the values it took.
- The **capacity** of a list or bytes is the number of elements its memory holds: 0 after `empty`, and `len` after `zeroed(len, alloc)`. When `push`, `insert` or `append` needs a length beyond the capacity, or `reserve(n)` needs the length plus n, the new capacity is the largest of twice the old capacity, the needed length and 4. A capacity whose size in bytes exceeds 2^64 − 1 is exhaustion. A capacity never decreases.
- A pool's `reserve(n)` makes its next n inserts draw no memory.
- Removing elements never draws memory.
- `Array` ([arrays and slices](04-types.md#arrays-and-slices)), `mem::Bounded[T, N]` ([bounded lists](#bounded-lists)) and `text::Fixed[N]` store their elements inline, hold no allocator and never draw memory.

```nan
pub fn fill(mut out: mem::List[u8], count: usize) {
    for _ in 0..<count {
        mut out.push(0) // traps: trap.exhausted
    }
} fill

test fill_beyond_budget for fill {
    let budget = testing::world().alloc.split(65_536)
    var bytes = mem::List[u8].empty(budget.alloc())
    fill(mut bytes, count: 100_000)
    take budget.close()
} fill_beyond_budget
```

## Stores

A **store** is a manager of values in memory: `mem::Heap`, `mem::Arena[T]` or `mem::Pool[T, B]`. Each draws chunks through the allocator it was created with, and each makes [boxes](07-ownership.md#box):

- `boxed(take value)` moves `value` into a new box in memory drawn through the store's allocator. `try_boxed` is its failing form and takes only droppable values. A box's memory returns when the box is released, whether or not the store still exists.
- `over(take region)` creates the store over a physical region ([regions](#regions)).
- Releasing an arena or pool releases every entry it holds and returns their memory.

### Heaps

`mem::Heap` is the Copy store of boxes: it owns nothing but its allocator.

```nan-api
// module: mem

pub open struct Heap {
} Heap

pub fn Heap.of(alloc: Alloc) -> Heap

pub fn Heap.over(take region: hw::Region) -> Heap

pub fn Heap.alloc(self) -> Alloc

pub fn Heap.boxed[T: Any](self, take value: T) -> Box[T]

pub fn Heap.try_boxed[T](self, take value: T) -> Box[T] fails OutOfMemory
```

```nan
pub closed struct Table {
    pub slots: Array[u64, 512]
    pub used: usize
} Table

pub fn empty_table(alloc: mem::Alloc) -> mem::Box[Table] {
    let heap = mem::Heap.of(alloc)
    return heap.boxed(Table(slots: Array[u64, 512].filled(0), used: 0))
} empty_table
```

### Arenas

`mem::Arena[T]` is an insert-only store whose entries never move and are released only with the arena. `mem::Id[T]` is the index of an entry: the n-th push returns the id with index n − 1.

```nan-api
// module: mem

pub open struct Arena[T] {
} Arena

pub fn Arena.empty(alloc: Alloc) -> Arena[T]

pub fn Arena.over(take region: hw::Region) -> Arena[T]

pub fn Arena.len(self) -> usize limits notrap, terminates

pub fn Arena.push(self, take value: T) -> Id[T]

pub fn Arena.try_push(self, take value: T) -> Id[T] fails OutOfMemory

pub proj fn Arena.at(self, id: Id[T]) -> T

pub proj fn Arena.at_mut(mut self, id: Id[T]) -> T

pub proj fn Arena.get(self, id: Id[T]) -> Option[T]

pub proj fn Arena.cursor(self) -> ArenaCursor[T]

pub fn Arena.boxed[U: Any](self, take value: U) -> Box[U]

pub fn Arena.try_boxed[U](self, take value: U) -> Box[U] fails OutOfMemory

pub open struct Id[T] {
} Id

pub fn Id.of(index: u32) -> Id[T] limits pure, notrap, terminates

pub fn Id.index(self) -> u32 limits pure, notrap, terminates
```

- `push` takes the arena by read: the arena has [interior mutability](#interior-mutability), and views of earlier entries stay live across it. Pushes from several tasks are each atomic, and ids follow the order in which they complete. A push beyond 2^32 entries is exhaustion.
- An id is a plain index, made from any `u32` by `mem::Id.of`. `arena[id]` calls `at`, which traps when the index is not below `len()` (`trap.bounds`); `get` returns `None` there. An id from another arena of the same `T` reaches that arena's entry at its index.
- For every `T`, `mem::Id[T]` implements `Eq`, `Ord` and `Hash` as its `u32` index does, `Encode` as [std encodings](13-text-encode.md#std-encodings) states, and `Generate` by generating that index.
- `cursor` yields the entries that exist when it is created, in push order.

```nan
pub closed struct Symbol derives Copy, Eq {
    pub hash: u64
    pub arity: u8
} Symbol

pub fn declare_pair(symbols: mem::Arena[Symbol], hash: u64) -> u8 {
    let left = symbols.push(Symbol(hash: hash, arity: 1))
    let right = symbols.push(Symbol(hash: hash.wrapping_add(1), arity: 2))
    return symbols[left].arity + symbols[right].arity
} declare_pair
```

### Pools

`mem::Pool[T, B]` is a store of removable entries reached through generational handles `mem::Handle[T, B]`. `B` is a **brand**: a droppable type of which the pool holds no value, giving pools of the same `T` distinct handle types.

```nan-api
// module: mem

pub open struct Pool[T, B] {
} Pool

pub fn Pool.empty(alloc: Alloc) -> Pool[T, B]

pub fn Pool.over(take region: hw::Region) -> Pool[T, B]

pub fn Pool.len(self) -> usize limits pure, notrap, terminates

pub fn Pool.contains(self, handle: Handle[T, B]) -> bool limits pure, notrap, terminates

pub fn Pool.insert(mut self, take value: T) -> Handle[T, B]

pub fn Pool.try_insert(mut self, take value: T) -> Handle[T, B] fails OutOfMemory

pub fn Pool.reserve(mut self, additional: usize)

pub fn Pool.try_reserve(mut self, additional: usize) fails OutOfMemory

pub proj fn Pool.at(self, handle: Handle[T, B]) -> T

pub proj fn Pool.at_mut(mut self, handle: Handle[T, B]) -> T

pub proj fn Pool.get(self, handle: Handle[T, B]) -> Option[T]

pub proj fn Pool.get_mut(mut self, handle: Handle[T, B]) -> Option[T]

pub fn Pool.remove(mut self, handle: Handle[T, B]) -> T

pub proj fn Pool.cursor(self) -> PoolCursor[T, B]

pub fn Pool.boxed[U: Any](self, take value: U) -> Box[U]

pub fn Pool.try_boxed[U](self, take value: U) -> Box[U] fails OutOfMemory

pub open struct Handle[T, B] {
} Handle
```

- `insert` places the value in the free slot freed earliest, or in a new slot when none is free, and returns a live handle; an insert needing a new slot when the pool has 2^32 slots is exhaustion. `remove` moves the entry out and frees its slot, making every handle to it stale.
- A handle is stale for a pool when its index names no slot of that pool or its generation differs from the slot's. `pool[h]` calls `at` or `at_mut`. `at`, `at_mut` and `remove` trap on a stale handle (`trap.stale-handle`); `get`, `get_mut` and `contains` report it instead.
- A handle is made only by `insert` and `try_insert`. A handle from another pool of the same type reaches that pool's slot at its index when the generations agree.
- `mem::Handle[T, B]` implements `Eq` and `Hash` for every `T` and `B`: two handles are equal when their indexes and generations are, and hashing writes the index, then the generation, each as a `u32`. It implements no other interface.
- Growth may move entries; no view of an entry is live across it, since `insert` takes the pool by `mut`.
- `cursor` yields the live entries in slot order.

```nan
pub closed struct Timer derives Copy {
    pub deadline: u64
} Timer

pub closed struct TimerSlots {
} TimerSlots

pub fn postpone(mut timers: mem::Pool[Timer, TimerSlots], handle: mem::Handle[Timer, TimerSlots], delay: u64) -> bool {
    if not timers.contains(handle) {
        return false
    }
    timers[handle].deadline += delay
    return true
} postpone
```

```nan
pub closed struct Timer derives Copy {
    pub deadline: u64
} Timer

struct TimerSlots {
} TimerSlots

pub fn deadline_after_cancel(alloc: mem::Alloc) -> u64 {
    var timers = mem::Pool[Timer, TimerSlots].empty(alloc)
    let handle = mut timers.insert(Timer(deadline: 500))
    discard mut timers.remove(handle)
    return timers[handle].deadline // traps: trap.stale-handle
} deadline_after_cancel

test removed_timer_is_stale for deadline_after_cancel {
    let deadline = deadline_after_cancel(testing::world().alloc)
    expect deadline == 500
} removed_timer_is_stale
```

```nan
struct Jobs {
} Jobs

pub fn entries_in_slot_order(alloc: mem::Alloc) -> u32
    example entries_in_slot_order(testing::world().alloc) == 524
{
    var jobs = mem::Pool[u32, Jobs].empty(alloc)
    let earliest = mut jobs.insert(1)
    discard mut jobs.insert(2)
    let latest = mut jobs.insert(3)
    discard mut jobs.remove(latest)
    discard mut jobs.remove(earliest)
    discard mut jobs.insert(4)
    discard mut jobs.insert(5)
    var digits: u32 = 0
    for job in jobs {
        digits = digits * 10 + job
    }
    return digits
} entries_in_slot_order
```

### Regions

`over(take region)` consumes an `hw::Region` ([physical memory](24-kernel.md#physical-memory)) and adds a root cell whose supply is exactly the 64 KiB-aligned chunks lying wholly inside the region and whose limit is their total size. The store's allocator names that cell. Memory of the region outside those chunks is never used, and the region's memory never returns to the platform: a chunk returning to the cell becomes available to that cell's draws again.

```nan
// file: kernel/package.nan
//! Kernel memory setup.

package kernel {
    language 1
    targets x86_64-bare
} kernel

// file: kernel/kernel.nan
//! Boot-time heap over one physical region.

pub fn region_heap(take region: hw::Region) -> mem::Heap {
    return mem::Heap.over(take region)
} region_heap
```

## Chunk ownership

Every chunk belongs to exactly one domain: the domain whose task draws it, until [domain memory](11-errors-domains.md#domain-memory) passes it to another, charging it to that domain's cell when the two cells draw from one supply and that cell's chain has room for it.

- An allocation lies in chunks of the domain whose task makes it until [domain memory](11-errors-domains.md#domain-memory) passes them, so allocations share a chunk only when tasks of one domain make them through allocators naming one cell.
- When a domain is freed, each chunk it owns returns as [budgets](#budgets) states, whatever allocations it still holds.

## Shared values

`mem::Shared[T]` holds one `T` in a box with an atomic count of the `mem::Shared` values sharing it, and the allocator given to `of`.

```nan-api
// module: mem

pub open struct Shared[T] {
} Shared

pub fn Shared.of(take value: T, alloc: Alloc) -> Shared[T]

pub fn Shared.try_of(take value: T, alloc: Alloc) -> Shared[T] fails OutOfMemory

pub fn Shared.share(self) -> Shared[T] limits notrap, terminates, noalloc

pub proj fn Shared.contents(self) -> T limits pure, notrap, terminates

pub proj fn Shared.make_mut(mut self, clone: fn(value: T, alloc: Alloc) -> T) -> T
```

- `share` returns another `mem::Shared` of the same contents and adds 1 to the count. Releasing a `mem::Shared` subtracts 1; the one that brings the count to 0 releases the contents and the box.
- The count is not part of the contents: `share` and release change it atomically and through no access to the contents. The contents are read only through `contents` and written only through `make_mut`.
- Each release of a `mem::Shared` synchronises with the `make_mut` or release that later observes the count it left ([memory model](23-low-level.md#memory-model)).
- `make_mut` yields a writable view of the contents. When the count is above 1 it first calls `clone` with a read of the contents and the stored allocator, moves the result into a new box drawn through that allocator, trapping on exhaustion (`trap.exhausted`), makes the receiver hold that box with a count of 1, and subtracts 1 from the old count, releasing the old contents and box when that brings it to 0. When the count is 1 it calls nothing.

```nan
pub closed struct Playlist {
    pub tracks: mem::List[u32]
} Playlist

fn copy_playlist(value: Playlist, alloc: mem::Alloc) -> Playlist {
    var tracks = mem::List[u32].empty(alloc)
    for track in value.tracks {
        mut tracks.push(track)
    }
    return Playlist(tracks: take tracks)
} copy_playlist

pub fn add_track(mut shared: mem::Shared[Playlist], track: u32) {
    var playlist = mut shared.make_mut(clone: copy_playlist)
    mut playlist.tracks.push(track)
} add_track
```

## Collections

The collections are `mem::List[T]`, `mem::Bytes`, `mem::Map[K, V]`, `mem::HashMap[K, V]`, `mem::Set[T]` and `mem::Bounded[T, N]`. Each is created empty with `empty` and has `len`.

- A collection implements `Eq` on the instantiations whose element types, or key and value types, implement `Eq`, and `Hash` likewise ([equality, ordering, hashing and defaults](08-generics-interfaces.md#equality-ordering-hashing-and-defaults)). Two collections are equal when their lengths are and their elements, or keys and values, are pairwise equal in iteration order. `mem::Bytes` also implements `Ord` as `Slice[u8]` does. A list, bytes, bounded list or set hashes as a `Slice` of its elements in iteration order; a map or hash map writes its length as a `u64` in 8 bytes little-endian, then each key's hash followed by its value's, in iteration order.
- `cursor`, `cursor_mut` and `into_cursor` serve the three forms of `for` ([iteration](07-ownership.md#iteration)); [memory types](#memory-types) lists the cursors. A collection or store without one of these functions supports no `for` of that form.
- Each call of `at` or `at_mut` of a list, bytes or bounded list has the index-bound obligation that [indexing](07-ownership.md#indexing) states. `get` returns `None` where that bound fails.
- The `cursor` and `cursor_mut` of a map or hash map yield `mem::Entry[K, V]`, a view of one stored entry: `key` and `value` project its key and value, and `value_mut` projects its value writably. An entry that a [read cursor](07-ownership.md#iteration) yields is read-only ([projection results](07-ownership.md#projection-results)). No operation writes a stored key.

```nan-api
// module: mem

pub open view struct Entry[K, V] {
} Entry

pub proj fn Entry.key(self) -> K

pub proj fn Entry.value(self) -> V

pub proj fn Entry.value_mut(mut self) -> V

pub closed struct KeyValue[K, V] {
    pub key: K
    pub value: V
} KeyValue
```

### Lists and bytes

`mem::List[T]` is a growable sequence stored contiguously. `mem::Bytes` is a growable sequence of `u8`, the owned form of byte data.

```nan-api
// module: mem

pub open struct List[T: Any] {
} List

pub fn List.empty(alloc: Alloc) -> List[T]

pub fn List.len(self) -> usize limits pure, notrap, terminates

pub fn List.capacity(self) -> usize limits pure, notrap, terminates

pub fn List.push(mut self, take value: T)
    ensures self.len() > 0 and self.len() - 1 == old(self.len())

pub fn List.try_push(mut self, take value: T) fails OutOfMemory
    ensures self.len() > 0 and self.len() - 1 == old(self.len())

pub fn List.insert(mut self, i: usize, take value: T)
    requires i <= self.len()

pub fn List.try_insert(mut self, i: usize, take value: T) fails OutOfMemory
    requires i <= self.len()

pub fn List.reserve(mut self, additional: usize)

pub fn List.try_reserve(mut self, additional: usize) fails OutOfMemory

pub fn List.pop(mut self) -> Option[T]

pub fn List.remove(mut self, i: usize) -> T
    requires i < self.len()

pub fn List.truncate(mut self, len: usize)

pub proj fn List.at(self, i: usize) -> T limits pure, notrap, terminates

pub proj fn List.at_mut(mut self, i: usize) -> T limits pure, notrap, terminates

pub proj fn List.get(self, i: usize) -> Option[T]

pub proj fn List.view(self) -> Slice[T]

pub proj fn List.view_mut(mut self) -> SliceMut[T]

pub proj fn List.cursor(self) -> core::SliceCursor[T]

pub proj fn List.cursor_mut(mut self) -> core::SliceCursorMut[T]

pub fn List.into_cursor(take self) -> ListCursor[T]

pub fn List.retain(mut self, keep: fn(value: T) -> bool)

pub fn List.position(self, matches: fn(value: T) -> bool) -> Option[usize]

pub fn List.sort_by(mut self, order: fn(left: T, right: T) -> core::Ordering)

pub open struct Bytes {
} Bytes

pub fn Bytes.empty(alloc: Alloc) -> Bytes

pub fn Bytes.zeroed(len: usize, alloc: Alloc) -> Bytes

pub fn Bytes.try_zeroed(len: usize, alloc: Alloc) -> Bytes fails OutOfMemory

pub fn Bytes.len(self) -> usize limits pure, notrap, terminates

pub fn Bytes.capacity(self) -> usize limits pure, notrap, terminates

pub fn Bytes.push(mut self, byte: u8)

pub fn Bytes.try_push(mut self, byte: u8) fails OutOfMemory

pub fn Bytes.append(mut self, data: Slice[u8])

pub fn Bytes.try_append(mut self, data: Slice[u8]) fails OutOfMemory

pub fn Bytes.reserve(mut self, additional: usize)

pub fn Bytes.try_reserve(mut self, additional: usize) fails OutOfMemory

pub fn Bytes.truncate(mut self, len: usize)

pub proj fn Bytes.at(self, i: usize) -> u8 limits pure, notrap, terminates

pub proj fn Bytes.at_mut(mut self, i: usize) -> u8 limits pure, notrap, terminates

pub proj fn Bytes.view(self) -> Slice[u8]

pub proj fn Bytes.view_mut(mut self) -> SliceMut[u8]

pub proj fn Bytes.cursor(self) -> core::SliceCursor[u8]

pub proj fn Bytes.cursor_mut(mut self) -> core::SliceCursorMut[u8]
```

- `truncate`, `retain`, `try_push` and `try_insert` [count `T` as droppable](05-properties.md#generic-parameters): calling one on a list whose `T` is linear is an error (`prop.linear-argument`).
- `insert` and `remove` shift the later elements by one place; `pop` removes the last element, or returns `None` on an empty list. `truncate(len)` releases the elements from index `len` on, and does nothing when `len` is not below `len()`.
- `zeroed(len, alloc)` is a `mem::Bytes` of `len` zero bytes; `append` appends a copy of `data`.

```nan
pub closed linear struct Lease {
    pub slot: u32
} Lease

pub fn renew(mut leases: mem::List[Lease], take lease: Lease) {
    mut leases.push(take lease)
    mut leases.truncate(1) // error: prop.linear-argument
} renew
```

### Maps and sets

`mem::Map[K: Ord, V]` maps keys to values and iterates in ascending key order. `mem::HashMap[K: Eq and Hash, V]` maps keys to values by hashing and iterates in insertion order. `mem::Set[T: Ord]` holds distinct values and iterates in ascending order.

```nan-api
// module: mem

pub open struct Map[K: Ord, V] {
} Map

pub fn Map.empty(alloc: Alloc) -> Map[K, V]

pub fn Map.len(self) -> usize limits pure, notrap, terminates

pub fn Map.contains(self, key: K) -> bool limits pure, notrap, terminates

pub fn Map.insert(mut self, take key: K, take value: V) -> Option[V]

pub fn Map.try_insert(mut self, take key: K, take value: V) -> Option[V] fails OutOfMemory

pub fn Map.remove(mut self, key: K) -> Option[V]

pub proj fn Map.get(self, key: K) -> Option[V]

pub proj fn Map.get_mut(mut self, key: K) -> Option[V]

pub proj fn Map.at(self, key: K) -> V
    requires self.contains(key)

pub proj fn Map.at_mut(mut self, key: K) -> V
    requires self.contains(key)

pub proj fn Map.cursor(self) -> MapCursor[K, V]

pub proj fn Map.cursor_mut(mut self) -> MapCursorMut[K, V]

pub fn Map.into_cursor(take self) -> MapIntoCursor[K, V]

pub fn Map.retain(mut self, keep: fn(key: K, value: V) -> bool)

pub open struct HashMap[K: Eq and Hash, V] {
} HashMap

pub fn HashMap.empty(seed: u64, alloc: Alloc) -> HashMap[K, V]

pub fn HashMap.len(self) -> usize limits pure, notrap, terminates

pub fn HashMap.contains(self, key: K) -> bool limits pure, notrap, terminates

pub fn HashMap.insert(mut self, take key: K, take value: V) -> Option[V]

pub fn HashMap.try_insert(mut self, take key: K, take value: V) -> Option[V] fails OutOfMemory

pub fn HashMap.remove(mut self, key: K) -> Option[V]

pub proj fn HashMap.get(self, key: K) -> Option[V]

pub proj fn HashMap.get_mut(mut self, key: K) -> Option[V]

pub proj fn HashMap.at(self, key: K) -> V
    requires self.contains(key)

pub proj fn HashMap.at_mut(mut self, key: K) -> V
    requires self.contains(key)

pub proj fn HashMap.cursor(self) -> HashMapCursor[K, V]

pub proj fn HashMap.cursor_mut(mut self) -> HashMapCursorMut[K, V]

pub fn HashMap.into_cursor(take self) -> HashMapIntoCursor[K, V]

pub fn HashMap.retain(mut self, keep: fn(key: K, value: V) -> bool)

pub open struct Set[T: Ord] {
} Set

pub fn Set.empty(alloc: Alloc) -> Set[T]

pub fn Set.len(self) -> usize limits pure, notrap, terminates

pub fn Set.contains(self, value: T) -> bool limits pure, notrap, terminates

pub fn Set.insert(mut self, take value: T) -> bool

pub fn Set.try_insert(mut self, take value: T) -> bool fails OutOfMemory

pub fn Set.remove(mut self, value: T) -> bool

pub proj fn Set.cursor(self) -> SetCursor[T]

pub fn Set.into_cursor(take self) -> SetIntoCursor[T]

pub fn Set.retain(mut self, keep: fn(value: T) -> bool)
```

- `mem::Map` and `mem::Set` order and match keys by `cmp`. `mem::HashMap` and `mem::OnceMap` match keys by `eq` and place them by the `hash` of each key written to a hasher keyed by `seed`, so `seed` affects their layout in memory and never their iteration order.
- `insert` with a new key adds the entry and returns `None`. With a present key it keeps the stored key and its position, replaces the value and returns `Some` of the old value, releasing the key it took. `remove` releases the stored key and returns the removed value, or `None`.
- `at` and `at_mut` project the value of `key`, and `map[key]` calls them ([indexing](07-ownership.md#indexing)).
- `Set.insert` returns `true` when it added the value and `false` when the value was present, releasing it; `Set.remove` returns whether it removed one.
- Removing an entry of a `mem::HashMap` keeps the order of the others; reinserting a removed key places it last.

```nan
pub fn tally(words: Slice[u32], seed: u64, alloc: mem::Alloc) -> mem::HashMap[u32, u32] {
    var counts = mem::HashMap[u32, u32].empty(seed, alloc)
    for word in words {
        if counts.contains(word) {
            counts[word] += 1
        } else {
            discard mut counts.insert(word, value: 1)
        }
    }
    return counts
} tally
```

### Bounded lists

`mem::Bounded[T, N]` is a list of at most `N` elements stored inline, holding no allocator.

```nan-api
// module: mem

pub open struct Bounded[T, N: usize where N > 0] {
} Bounded

pub fn Bounded.empty() -> Bounded[T, N] limits pure, notrap, terminates

pub fn Bounded.len(self) -> usize limits pure, notrap, terminates

pub fn Bounded.is_full(self) -> bool = self.len() == N

pub fn Bounded.push(mut self, take value: T) limits notrap
    requires not self.is_full()

pub fn Bounded.try_push(mut self, take value: T) fails OutOfMemory limits notrap

pub fn Bounded.pop(mut self) -> Option[T] limits notrap

pub proj fn Bounded.at(self, i: usize) -> T limits pure, notrap, terminates

pub proj fn Bounded.at_mut(mut self, i: usize) -> T limits pure, notrap, terminates

pub proj fn Bounded.view(self) -> Slice[T]

pub proj fn Bounded.view_mut(mut self) -> SliceMut[T]

pub proj fn Bounded.cursor(self) -> core::SliceCursor[T]

pub proj fn Bounded.cursor_mut(mut self) -> core::SliceCursorMut[T]

pub fn Bounded.into_cursor(take self) -> BoundedCursor[T, N]
```

- `push` appends `value`; a push on a full bounded list breaks its requires. `try_push` fails with `mem::OutOfMemory.Exhausted` there, leaving the list unchanged and releasing `value`. `pop` removes the last element, or returns `None` on an empty bounded list.

```nan
pub fn first_errors(codes: Slice[u16]) -> mem::Bounded[u16, 4] {
    var kept = mem::Bounded[u16, 4].empty()
    for code in codes {
        if kept.is_full() {
            break
        }
        if code >= 400 {
            mut kept.push(code)
        }
    }
    return kept
} first_errors
```

### Eager methods

An **eager method** is a std method with a parameter of a function type other than `task fn` and `interrupt fn`: it calls the function only during its own call and keeps nothing. No std method returns a sequence computed later. That parameter is second-class ([function values](07-ownership.md#function-values)), and the digest of the function supplied is charged to the call supplying it ([function arguments](10-effects-digests.md#function-arguments)).

- `retain` calls `keep` once per element in iteration order and releases the elements for which it returns `false`, keeping the order of the others.
- `position` calls `matches` in index order until it returns `true`, and returns that index, or `None`.
- `sort_by` draws no memory and runs these steps, n being the list's `len`: for each i from 1 to n − 1 in order, it takes the element x at index i, sets `lo` to 0 and `hi` to i, and while `lo < hi` calls `order(x, e)` with e the element at index `mid`, `(lo + hi) / 2` rounded down, `Less` setting `hi` to `mid` and `Equal` or `Greater` setting `lo` to `mid + 1`; it then moves x to index `lo`, the elements from `lo` to i − 1 moving up by one. The sort is stable.

```nan
pub fn drop_quiet(mut levels: mem::List[u32], floor: u32) {
    mut levels.retain(keep: fn(value: u32) => value >= floor)
} drop_quiet
```

## Interior mutability

**Interior mutability** is mutation of a value's contents through a read access. Exactly these base types have it: `task::Cown[T]` ([cowns](16-concurrency.md#cowns)), the [atomic](16-concurrency.md#atomics) types of `core::atomic`, `mem::Arena[T]`, `mem::OnceMap[K, V]`, `text::Interner` ([interning](13-text-encode.md#interning)), `hw::SpinLock[T]` and `hw::IrqShared[T]` ([locks](24-kernel.md#locks)), `hw::PerCpu[T]` ([scheduling](24-kernel.md#scheduling)) and `hw::Signal[T]` ([signals](24-kernel.md#signals)). A type containing one of them has interior mutability through it; no other type has it.

- Each of these types is implemented by trusted code in std, and never moves or frees a value it holds while a view of that value may be live.
- Each operation on one of them is atomic with respect to every other task.
- The [frame rule](20-prover.md#frame-rule) states which places within them a call may change.

### Once maps

`mem::OnceMap[K, V]` is a growable map whose entries are inserted through a read access and never replaced or removed.

```nan-api
// module: mem

pub open struct OnceMap[K: Eq and Hash, V] {
} OnceMap

pub fn OnceMap.empty(seed: u64, alloc: Alloc) -> OnceMap[K, V]

pub fn OnceMap.len(self) -> usize limits notrap, terminates

pub proj fn OnceMap.get(self, key: K) -> Option[V]

pub proj fn OnceMap.insert(self, take key: K, take value: V) -> V

pub proj fn OnceMap.try_insert(self, take key: K, take value: V) -> V fails OutOfMemory
```

- `insert` stores the entry when `key` is absent and yields a view of the stored value. When `key` is present, it releases the key and value it took and yields the value already stored. Concurrent inserts of one key store exactly one value.
- Views of values stay valid across later inserts, since entries never move.

```nan
pub fn cached_square(cache: mem::OnceMap[u32, u64], key: u32) -> u64 {
    let Some(hit) = cache.get(key) else {
        let square = u64.widen(key) * u64.widen(key)
        return cache.insert(key, value: square)
    }
    return hit
} cached_square
```

## Memory types

Every type this chapter declares other than `mem::OutOfMemory` and `mem::KeyValue` is a [base type](05-properties.md#structural-properties) with these properties. The primitive capabilities among them are `mem::Alloc`, `mem::Budget` and `mem::Heap`. "as" means exactly when every type argument other than a brand has the property; a brand affects no property.

| Type | linear | view | Copy | Send | Inert | AnyBits |
| --- | --- | --- | --- | --- | --- | --- |
| `mem::Alloc`, `mem::Heap` | no | no | yes | yes | no | no |
| `mem::Budget` | yes | no | no | yes | no | no |
| `mem::Id[T]` | no | no | yes | yes | yes | yes |
| `mem::Handle[T, B]` | no | no | yes | yes | yes | no |
| `mem::Arena[T]`, `mem::OnceMap[K, V]` | no | no | no | as | no | no |
| `mem::List[T]`, `mem::ListCursor[T]` | as | no | no | as | as | no |
| `mem::Pool[T, B]`, `mem::Shared[T]`, `mem::Map[K, V]`, `mem::HashMap[K, V]`, `mem::Set[T]` and the other owning cursors | no | no | no | as | as | no |
| `mem::Bytes` | no | no | no | yes | yes | no |
| `mem::Bounded[T, N]` | no | no | as | as | as | no |
| `mem::Entry[K, V]` and the view cursors | no | yes | no | as | as | no |

- Every type parameter of these types other than the `T` of `mem::List` and `mem::ListCursor` is droppable ([generic parameters](05-properties.md#generic-parameters)).
- The allocator stored by a growable container or by `mem::Shared` is no capability for [Inert](05-properties.md#inert).
- `mem::Bounded[T, N]` and `mem::BoundedCursor[T, N]` hold `T` inline; every other base type of this chapter holds nothing inline ([structs](04-types.md#structs)).

The **view cursors** of `mem`, returned by `cursor` and `cursor_mut`, implement `core::Cursor` ([iteration](07-ownership.md#iteration)) with a `next` that is a projection; those of `cursor_mut` yield writable entries. Those that `cursor` returns are [read cursors](07-ownership.md#iteration). The **owning cursors**, returned by `into_cursor`, have a `next` that is not a projection and yields owned values; an owning cursor implements `core::Cursor` exactly for the instantiations whose element type, or key and value types, are droppable, its impl line applying to no other ([impl lines](08-generics-interfaces.md#impl-lines)), and releasing an owning cursor releases the elements it has not yielded. Every cursor yields in the iteration order of what it was made from, which for lists and bounded lists is index order.

```nan-api
// module: mem

pub open view struct ArenaCursor[T] {
} ArenaCursor

impl ArenaCursor[T]: core::Cursor[T]

pub proj fn ArenaCursor.next(mut self) -> Option[T]

pub open view struct PoolCursor[T, B] {
} PoolCursor

impl PoolCursor[T, B]: core::Cursor[T]

pub proj fn PoolCursor.next(mut self) -> Option[T]

pub open struct ListCursor[T: Any] {
} ListCursor

impl ListCursor[T]: core::Cursor[T]

pub fn ListCursor.next(mut self) -> Option[T]

pub open struct BoundedCursor[T, N: usize where N > 0] {
} BoundedCursor

impl BoundedCursor[T, N]: core::Cursor[T]

pub fn BoundedCursor.next(mut self) -> Option[T]

pub open view struct MapCursor[K, V] {
} MapCursor

impl MapCursor[K, V]: core::Cursor[Entry[K, V]]

pub proj fn MapCursor.next(mut self) -> Option[Entry[K, V]]

pub open view struct MapCursorMut[K, V] {
} MapCursorMut

impl MapCursorMut[K, V]: core::Cursor[Entry[K, V]]

pub proj fn MapCursorMut.next(mut self) -> Option[Entry[K, V]]

pub open struct MapIntoCursor[K, V] {
} MapIntoCursor

impl MapIntoCursor[K, V]: core::Cursor[KeyValue[K, V]]

pub fn MapIntoCursor.next(mut self) -> Option[KeyValue[K, V]]

pub open view struct HashMapCursor[K, V] {
} HashMapCursor

impl HashMapCursor[K, V]: core::Cursor[Entry[K, V]]

pub proj fn HashMapCursor.next(mut self) -> Option[Entry[K, V]]

pub open view struct HashMapCursorMut[K, V] {
} HashMapCursorMut

impl HashMapCursorMut[K, V]: core::Cursor[Entry[K, V]]

pub proj fn HashMapCursorMut.next(mut self) -> Option[Entry[K, V]]

pub open struct HashMapIntoCursor[K, V] {
} HashMapIntoCursor

impl HashMapIntoCursor[K, V]: core::Cursor[KeyValue[K, V]]

pub fn HashMapIntoCursor.next(mut self) -> Option[KeyValue[K, V]]

pub open view struct SetCursor[T] {
} SetCursor

impl SetCursor[T]: core::Cursor[T]

pub proj fn SetCursor.next(mut self) -> Option[T]

pub open struct SetIntoCursor[T] {
} SetIntoCursor

impl SetIntoCursor[T]: core::Cursor[T]

pub fn SetIntoCursor.next(mut self) -> Option[T]
```

## Diagnostics

The `mem` family has no codes.
