# Low-level programming

This chapter defines trusted code and its premises, trusted primitives and their structured requires, safe code and undefined behaviour, `hw::Ptr[T]`, the address types and `UserAddr.copy_in`, layout items, registers items with the register windows `hw::Mmio[R]` and `hw::Ports[R]`, DMA buffers and their ordering, privileged intrinsics and install tokens, machine seams and compiler stubs, landing fns and `hw::mint_root`, foreign fns and calling conventions, the memory model, and the properties of the hardware types.

## Trusted code

**Trusted code** is a `trusted "premise" { ... }` block or expression. Its **premise** is the text after `trusted`, stating what the code relies on beyond the rules the checker enforces.

```ebnf
trusted_expr = "trusted" TEXT "{" trusted_body "}" ;
trusted_body = expr ( ( assign_operator expr )? NL stmt* )? | trusted_lead_stmt stmt* ;
trusted_lead_stmt = let_stmt | var_stmt | discard_stmt | while_stmt | for_stmt | loop_stmt
    | defer_stmt | undefer_stmt | expect_stmt | assume_stmt ;
```

- A body that is one `expr` alone makes a trusted expression, whose type and value are the expression's. Every other body makes a trusted block: a [block](06-statements-expressions.md#statements) whose statements run in order, whose bindings end at its `}` and whose type is `Unit`. A trusted block that is not a whole expression statement is an error (`trusted.block-value`).
- A premise that is empty or holds only spaces is an error (`trusted.empty-premise`).
- Trusted code is legal only in a module that an `allow trusted` [manifest grant](14-authority.md#manifest-grants) names; elsewhere it is an error (`authority.ungranted`).
- Trusted code written inside trusted code, including inside a closure there, is an error (`trusted.nested`). The body of a closure written inside trusted code is not trusted code.
- Trusted code runs exactly as the same code outside it would. It only permits what is illegal elsewhere: calls of [trusted primitives](#trusted-primitives) and [`assume`](18-contracts.md#assume).
- Each trusted code is an entry of the [ledger](31-trust.md#ledger) with its premise, and an item holding trusted code is a [trust escape](31-trust.md#trust-escapes).

```nan
// file: ring/package.nan
//! Byte copies for rings.

package ring {
    language 1
    targets any
    allow trusted in ring
} ring

// file: ring/ring.nan
//! Copies a prefix between byte views.

pub fn copy_prefix(source: Slice[u8], mut target: SliceMut[u8], count: usize)
    requires count <= source.len()
    requires count <= target.len()
{
    let from = hw::Ptr.of_view(source)
    let to = hw::Ptr.of_view_mut(mut target)
    trusted "both views hold at least count initialised elements" {
        from.copy(to, count)
    }
} copy_prefix

pub fn first_byte(source: Slice[u8]) -> u8
    requires source.len() > 0
{
    let start = hw::Ptr.of_view(source)
    return trusted "source is a live view of at least one byte" { start.read() }
} first_byte
```

### Trusted primitives

A **trusted primitive** is a std function callable only inside trusted code. The trusted primitives are exactly `hw::Ptr.read`, `write`, `offset`, `cast`, `copy` and the `atomic_` methods of `hw::Ptr` ([pointers](#pointers)), `hw::fence` ([trusted atomics](#trusted-atomics)), `hw::Mmio.map` and `hw::Ports.map` ([register windows](#register-windows)), `hw::Ring0.write_cr3`, `write_cr3_tagged`, `read_msr`, `write_msr`, `write_ttbr0`, `write_ttbr0_tagged`, `write_sysreg`, `invalidate_data_cache` and `firmware_call` ([privileged intrinsics](#privileged-intrinsics)), and `hw::TrapFrame.set_ip` ([interrupt functions](24-kernel.md#interrupt-functions)).

- A call of a trusted primitive outside trusted code, or its use as a function value, is an error (`trusted.primitive-outside`).
- Each primitive has a **soundness condition**, stated with its rules. Its **structured requires** are its `requires` clauses: the part of the condition that [contract terms](18-contracts.md#contract-terms) express. Each is a trusted requirement [obligation](18-contracts.md#obligations) at every call, which the prover proves, a run-time check enforces (`trap.requires`), or an `assume` in the enclosing trusted code makes assumed.
- The premise of the enclosing trusted code asserts the rest of the soundness condition.

```nan
pub fn first_word(words: Slice[u32]) -> u32
    requires words.len() > 0
{
    let start = hw::Ptr.of_view(words)
    return start.read() // error: trusted.primitive-outside
} first_word
```

## Safe code

**Safe code** is all code outside trusted code. **Undefined behaviour** is behaviour this specification does not define.

An execution has undefined behaviour exactly from the first point where one of these happens:

- trusted code breaks a [Ptr rule](#ptr-rules) or the soundness condition of a trusted primitive it calls;
- the condition of an `assume`, or the premise of trusted code, is false;
- a [foreign fn](#foreign-functions), or a function reached through a foreign function type, behaves otherwise than its head states;
- code performing `device` or `priv` operations programs the hardware against what its types state, such as a device accessing memory other than its DMA buffers, or a DMA buffer after the operation that handed it to the device has returned it.

No rule of this specification constrains an execution from that point on. In every other execution, each operation has the behaviour its owning rule defines. The [interpreter](21-interpreter.md#ptr-checks) checks the Ptr rules and every `assume` in the runs it makes.

## Pointers

`hw::Ptr[T]` is the address of a `T`. It has no dereference operator: trusted primitives read and write through it.

```nan-api
// module: hw

pub open struct Ptr[T: Any] {
} Ptr

pub fn Ptr.of_addr(addr: usize) -> Ptr[T] limits pure, notrap, terminates

pub fn Ptr.of(value: T) -> Ptr[T] limits pure, notrap, terminates

pub fn Ptr.of_mut(mut value: T) -> Ptr[T] limits pure, notrap, terminates

pub fn Ptr.of_view(items: Slice[T]) -> Ptr[T] limits pure, notrap, terminates

pub fn Ptr.of_view_mut(mut items: SliceMut[T]) -> Ptr[T] limits pure, notrap, terminates

pub fn Ptr.addr(self) -> usize limits pure, notrap, terminates

pub fn Ptr.is_aligned(self) -> bool limits pure, notrap, terminates

pub fn Ptr.read(self) -> T
    requires self.addr() != 0
    requires self.is_aligned()

pub fn Ptr.write(self, take value: T)
    requires self.addr() != 0
    requires self.is_aligned()

pub fn Ptr.offset(self, count: isize) -> Ptr[T]

pub fn Ptr.cast[U: Any](from: Ptr[U]) -> Ptr[T]

pub fn Ptr.copy(self, to: Ptr[T], count: usize)
    requires self.is_aligned()
    requires to.is_aligned()
```

- `hw::Ptr[T]` implements `Eq` and `Hash` by address. Its 8 bytes are the address.
- `of_addr` makes a pointer without provenance. `of`, `of_mut`, `of_view` and `of_view_mut` make a pointer to the place or to the first element of the view, with the provenance the [Ptr rules](#ptr-rules) give.
- `is_aligned()` is true exactly when the address is a multiple of the alignment of `T` in the final closure's [type layout](30-artefacts.md#type-layout).
- `read()` returns the value whose bytes lie at the address; for a type that is not Copy, the value read and the value in memory are two owners, and the premise states which one the program uses. `write(v)` stores v there without releasing the value it replaces.
- `offset(n)` returns the pointer n elements away, its address computed modulo 2^64, with the same provenance. `hw::Ptr[T].cast(p)` returns p's address and provenance as a pointer to `T`.
- `copy(to, n)` copies n elements from the receiver's address to `to`'s as if through a temporary, so the ranges may overlap.
- The soundness condition of each trusted primitive of this section is that the accesses and offsets it performs obey the Ptr rules.
- Every operation of this section terminates and allocates nothing. `read`, `write` and `copy` have the sync fact ([digests](10-effects-digests.md#digests)) and the others no effect fact. A trusted primitive's only trap sites are its structured requires.
- In [interpreter runs](21-interpreter.md#ptr-checks), `is_aligned` uses the interpreter layout, and an access through a pointer without provenance is uninterpretable ([run ends](21-interpreter.md#run-ends)).

## Address types

The address types are one-field types, one per address space:

```nan-api
// module: hw

pub closed struct PhysAddr derives Copy, Eq, Ord, Hash, Render {
    pub value: u64
} PhysAddr

pub closed struct VirtAddr derives Copy, Eq, Ord, Hash, Render {
    pub value: u64
} VirtAddr

pub closed struct BusAddr derives Copy, Eq, Ord, Hash, Render {
    pub value: u64
} BusAddr

pub open struct UserAddr[T: AnyBits] {
} UserAddr

pub fn UserAddr.of(value: u64) -> UserAddr[T] limits pure, notrap, terminates

pub fn UserAddr.value(self) -> u64 limits pure, notrap, terminates

pub fn UserAddr.copy_in(self, cpu: Ring0) -> Untrusted[T] fails UserAddrError

pub closed enum UserAddrError derives Copy, Eq, Render {
    OutsideUser
    Unmapped
} UserAddrError
```

- `hw::UserAddr[T]` holds one `u64`, the address of a `T` in the user address space of the page-table root the running CPU uses.
- The T of `hw::UserAddr[T]` has a [fixed layout](21-interpreter.md#interpreter-memory); another T is an error (`hw.layout-unfixed`), reported where the type is written.
- `addr.copy_in(cpu)` copies the bytes of a `T` from user memory in one fetch: it reads each byte exactly once and returns the copy as [Untrusted](15-untrusted.md#untrusted-values).
- It fails with `UserAddrError.OutsideUser` when the range from the address to the address plus the size of `T` does not lie below `0x0000_8000_0000_0000` on x86_64 and `0x0001_0000_0000_0000` on AArch64, and with `UserAddrError.Unmapped` when a page of the range is not mapped readable for user mode. Neither case traps.
- `copy_in` needs no manifest grant: holding `hw::Ring0` is its authority. It terminates, allocates nothing and has the sync fact ([digests](10-effects-digests.md#digests)).

```nan
// file: kernel/package.nan
//! System call arguments.

package kernel {
    language 1
    targets x86_64-bare
} kernel

// file: kernel/kernel.nan
//! Fetches request records from user memory.

pub layout Request: bytes 16 le {
    opcode: u32 at 0
    flags: u32 at 4
    length: u64 at 8
} Request

pub fn fetch_request(addr: hw::UserAddr[Request], cpu: hw::Ring0) -> Untrusted[Request] fails hw::UserAddrError {
    return try addr.copy_in(cpu)
} fetch_request
```

## Layouts

A **layout** is a `layout` item fixing the bit and byte representation of a value. Its head states the representation: an **integer layout** is represented by an unsigned integer type, a **bytes layout** by `bytes N`, N bytes. A **tags layout** is an integer layout whose values are listed tags.

```ebnf
layout_item = "layout" IDENT ":" layout_repr ( 'tags' "{" tag_line+ | "{" layout_member+ ) COL0 "}" item_label NL ;
layout_repr = 'bytes' INT layout_order? | IDENT layout_order? ;
layout_order = 'le' | 'be' ;
tag_line = ( DOC NL )* IDENT "=" INT NL ;
layout_member = ( DOC NL )* IDENT ":" layout_field NL ;
layout_field = 'bit' INT ( "..=" INT )? | 'rest' | type_expr 'at' INT ;
```

- The representation of an integer layout is `u8`, `u16`, `u32` or `u64`, N is at least 1, and a tags layout is an integer layout (`layout.repr`). Every `INT` of a layout item is an unsuffixed literal (`layout.literal`).
- A layout wider than one byte states its byte order, `le` or `be`, and a one-byte layout states none (`layout.endianness`); the exact fix writes `le` or deletes the order.
- A tags layout has at least one tag. Tag values fit the representation and are distinct (`layout.tag-value`). Tag `V` of layout `L` is the member `L.V`, a unit variant.
- Two fields or two tags of one layout with the same name are an error (`type.duplicate-member`).
- In an integer layout, a field is `bit i`, a `bool` occupying bit i; `bit i..=j`, occupying bits i to j as the smallest unsigned integer type holding j − i + 1 bits; `T at i`, where T is an unsigned integer type or an integer layout, occupying its width from bit i; or `rest`. Bit 0 is the least significant. A ranged field whose i is not below its j is an error (`layout.field-form`).
- In a bytes layout, a field is `T at k`, where T is an integer scalar, a layout or an `Array` of these, occupying its size from byte k. `bit` and `rest` fields in a bytes layout are errors (`layout.field-form`), and a field type that the layout's kind does not list is an error (`layout.field-type`).
- A field reaching beyond the representation is an error (`layout.out-of-range`), and two fields sharing a bit or byte, or two `rest` fields, are an error (`layout.overlap`).
- Every bit of an integer layout and every byte of a bytes layout is covered by exactly one field; a `rest` field covers every bit no other field covers. An uncovered bit or byte is an error (`layout.uncovered`), reported on the head line; for an integer layout the exact fix adds `reserved: rest`.
- Fields and tags have the visibility of their layout. A layout has no modifiers, generic parameters or predicates.

### Layout values

- A layout's value is the values of its fields, or one of its tags. Layout values are constructed, read, assigned and destructured as struct values are ([construction](04-types.md#construction), [patterns](04-types.md#patterns)), and tags are matched as variants.
- A `rest` field has the representation's type and holds the bits no other field covers; its other bits are 0. Constructing or assigning a ranged `bit` field with a value that does not fit its bits, or a `rest` field with a covered bit set, traps (`trap.conversion`), an obligation the prover proves or a run-time check enforces.
- The bits type of an integer layout is its representation and that of a bytes layout is `Array[u8, N]`; `to_bits`, `from_bits` and `checked_from_bits` convert ([bit patterns](09-numbers.md#bit-patterns)). The [niches](04-types.md#niches) of a layout are the bit patterns in which it, or a layout field at any depth, is a tags layout holding a value that it does not list.
- In memory, an integer layout is its bits value in its byte order, and a bytes layout is its fields at their offsets, whatever their alignment. Each multi-byte integer of a bytes layout, as a field or an array element, is in the layout's byte order, and each layout field is in its own. An integer layout aligns like its representation, a bytes layout to 1.
- Every layout implements `Eq` and `Hash` by its bits value.

```nan
pub const FRAME_LIMIT: u64 = 0x100_0000_0000

pub layout PageEntry: u64 le {
    present: bit 0
    writable: bit 1
    user: bit 2
    frame: bit 12..=51
    no_execute: bit 63
    reserved: rest
} PageEntry

pub fn PageEntry.mapping(frame: u64) -> PageEntry
    requires frame < FRAME_LIMIT
    example PageEntry.mapping(1).to_bits() == 0x1003
{
    return PageEntry(present: true, writable: true, user: false, frame: frame, no_execute: false, reserved: 0)
} PageEntry.mapping

pub layout Mode: u8 tags {
    Real = 0
    Long = 2
} Mode

pub layout Control: u16 le {
    mode: Mode at 0
    enabled: bit 15
    reserved: rest
} Control

pub fn Control.is_long(self) -> bool = self.mode == Mode.Long

pub fn Control.long(enabled: bool) -> Control
    example Control.long(true).to_bits() == 0x8002
{
    return Control(mode: Mode.Long, enabled: enabled, reserved: 0)
} Control.long

pub layout Gdtr: bytes 10 le {
    limit: u16 at 0
    base: u64 at 2
} Gdtr
```

```nan
pub layout LineStatus: u8 { // error: layout.uncovered
    data_ready: bit 0
    overrun: bit 1
} LineStatus
```

## Registers

A **registers block** is a `registers` item describing a device's registers: for each, its type, its offset in the block and its access. A **volatile access** is a read or write of one register through a [register window](#register-windows), never merged or reordered.

```ebnf
registers_item = "registers" IDENT "{" register_member+ COL0 "}" item_label NL ;
register_member = ( DOC NL )* register_line ;
register_line = 'bank' {!register.bank-reserved} | IDENT register_tail ;
register_tail = ":" type_expr 'at' INT ( 'stride' INT )? register_access NL ;
register_access = 'read' 'write'? | 'write' | 'write_one_clear' ;
```

- A member line starting with the word `bank` is an error (`register.bank-reserved`).
- Two registers of one registers item with the same name are an error (`type.duplicate-member`). Registers have the visibility of their registers item.
- A register's type, or an array register's element type, is AnyBits and 1, 2, 4 or 8 bytes long (`register.type`). Every `INT` is an unsuffixed literal (`register.literal`), and each offset is a multiple of the register's or element's size (`register.misaligned`).
- A register of type `Array[T, N]` is an array register whose element i lies at offset + i × stride. An array register states `stride S` with S a nonzero multiple of the element's size, and no other register states a stride (`register.stride`).
- The bytes of a register are those of its value, or of its elements for an array register. Registers whose bytes overlap are an error (`register.overlap`), unless one is `read` and the other `write`.
- A register is readable when its access is `read`, `read write` or `write_one_clear`, and writable when it is `write`, `read write` or `write_one_clear`. Writing a `write_one_clear` register clears exactly the bits written as 1, and reading it returns its bits.
- A registers item is a type without values. Using one as the type of a value, or instantiating `hw::Mmio[R]`, `hw::Ports[R]` or `hw::Register[R, T]` with an R that is neither a registers item nor a type parameter, is an error (`register.window-type`).
- The member path `R.f` of a register f of R is a value of type `hw::Register[R, T]`, where T is the register's type.

### Register windows

A **register window** is an `hw::Mmio[R]` (memory-mapped) or `hw::Ports[R]` (port I/O) reaching one registers block R.

```nan-api
// module: hw

pub open struct Register[R, T: AnyBits] {
} Register

pub open linear struct Mmio[R] {
} Mmio

pub fn Mmio.map(phys: u64) -> Mmio[R]

pub fn Mmio.unmap(take self)

pub fn Mmio.read[T: AnyBits](mut self, register: Register[R, T]) -> T

pub fn Mmio.write[T: AnyBits](mut self, register: Register[R, T], value: T)

pub fn Mmio.read_at[E: AnyBits, N: usize](mut self, register: Register[R, Array[E, N]], index: usize) -> E

pub fn Mmio.write_at[E: AnyBits, N: usize](mut self, register: Register[R, Array[E, N]], index: usize, value: E)

pub open linear struct Ports[R] {
} Ports

pub fn Ports.map(port: u16) -> Ports[R]

pub fn Ports.unmap(take self)

pub fn Ports.read[T: AnyBits](mut self, register: Register[R, T]) -> T

pub fn Ports.write[T: AnyBits](mut self, register: Register[R, T], value: T)

pub fn Ports.read_at[E: AnyBits, N: usize](mut self, register: Register[R, Array[E, N]], index: usize) -> E

pub fn Ports.write_at[E: AnyBits, N: usize](mut self, register: Register[R, Array[E, N]], index: usize, value: E)
```

- `map` is a trusted primitive whose soundness condition is that the window holds this machine's R registers, which for `Mmio` the virtual address `phys` reaches uncached in every address space where the window is used, and that no other live window maps them. `unmap` ends the window.
- A port window whose R has an 8-byte register or element is an error (`register.port-width`).
- Port I/O exists on x86_64 alone: a call of `hw::Ports.map` in code checked for an AArch64 target, or within the reach of a final build for one ([environments](27-program-model.md#environments)), is an error (`register.port-target`).
- A **volatile call** is a call of `read`, `write`, `read_at` or `write_at`. Its register argument is written as the member path `R.f` itself; any other argument there is an error (`register.operand`).
- `read` and `read_at` of a register that is not readable are an error (`register.not-readable`), and `write` and `write_at` of one that is not writable are an error (`register.not-writable`). `read` and `write` of an array register, and `read_at` and `write_at` of any other register, are errors (`register.array-access`).
- An index not below the array's length traps (`trap.bounds`).
- Each volatile call performs exactly one access of the register's or element's width: through `Mmio` at the virtual address `phys` plus the offset modulo 2^64, through `Ports` at the port `port` plus the offset modulo 65_536. A value travels as its memory representation.
- Volatile accesses of a task take effect in program order. The implementation never merges, splits, duplicates, removes or reorders them, nor moves them across [DMA access calls](#dma) and `hw::dma_fence`.
- `map`, `unmap` and volatile calls are device operations, legal only in modules that `allow device` names (`authority.ungranted`). Each has the device fact, terminates and allocates nothing; only an index traps. `hw::sim` constructors make simulated windows ([sim constructors](22-testing.md#sim-constructors)).

```nan
// file: kernel/package.nan
//! Serial console for a bare kernel.

package kernel {
    language 1
    targets x86_64-bare
    allow device in kernel
    allow trusted in kernel
} kernel

// file: kernel/kernel.nan
//! The serial port's registers.

const SERIAL_BASE: u64 = 0xFEB0_0000

const SCRATCH_SLOTS: usize = 4

pub layout LineStatus: u8 {
    data_ready: bit 0
    transmit_empty: bit 5
    reserved: rest
} LineStatus

pub layout IrqStatus: u32 le {
    received: bit 0
    sent: bit 1
    reserved: rest
} IrqStatus

pub registers SerialRegs {
    data: u8 at 0x0 read write
    line_status: LineStatus at 0x5 read
    irq_status: IrqStatus at 0x8 write_one_clear
    scratch: Array[u32, 4] at 0x10 stride 4 read write
} SerialRegs

pub fn map_serial() -> hw::Mmio[SerialRegs] {
    return trusted "the board decodes SERIAL_BASE to its 16550 alone and nothing else maps it" { hw::Mmio[SerialRegs].map(phys: SERIAL_BASE) }
} map_serial

pub fn send_byte(mut regs: hw::Mmio[SerialRegs], byte: u8) -> bool {
    let status = mut regs.read(SerialRegs.line_status)
    if not status.transmit_empty {
        return false
    }
    mut regs.write(SerialRegs.data, value: byte)
    return true
} send_byte

pub fn acknowledge_receive(mut regs: hw::Mmio[SerialRegs]) {
    let pending = mut regs.read(SerialRegs.irq_status)
    if pending.received {
        mut regs.write(SerialRegs.irq_status, value: IrqStatus(received: true, sent: false, reserved: 0))
    }
} acknowledge_receive

pub fn clear_scratch(mut regs: hw::Mmio[SerialRegs]) {
    for slot in 0..<SCRATCH_SLOTS {
        mut regs.write_at(SerialRegs.scratch, index: slot, value: 0)
    }
} clear_scratch
```

```nan
// file: kernel/package.nan
//! Serial console for a bare kernel.

package kernel {
    language 1
    targets x86_64-bare
    allow device in kernel
} kernel

// file: kernel/kernel.nan
//! The serial port's registers.

pub layout LineStatus: u8 {
    data_ready: bit 0
    reserved: rest
} LineStatus

pub registers SerialRegs {
    data: u8 at 0x0 read write
    line_status: LineStatus at 0x5 read
} SerialRegs

pub fn clear_status(mut regs: hw::Mmio[SerialRegs]) {
    mut regs.write(SerialRegs.line_status, value: LineStatus(data_ready: false, reserved: 0)) // error: register.not-writable
} clear_status
```

```nan
// file: kernel/package.nan
//! Serial console for an AArch64 bare kernel.

package kernel {
    language 1
    targets aarch64-bare
    allow device in kernel
    allow trusted in kernel
} kernel

// file: kernel/kernel.nan
//! The serial port's registers.

pub registers SerialRegs {
    data: u8 at 0x0 read write
} SerialRegs

pub fn map_serial() -> hw::Ports[SerialRegs] {
    return trusted "the board decodes port 0x3F8 to its 16550 alone and nothing else maps it" { hw::Ports[SerialRegs].map(0x3F8) } // error: register.port-target
} map_serial
```

## DMA

A **DMA buffer** is an `hw::Dma[T]` or an `hw::DmaShared[T]`: memory a device accesses at its bus address. An `hw::Dma[T]` is handed to a device whole: a submission takes it and a [completion](17-suspension-io.md#completion-io) returns it. An `hw::DmaShared[T]` is shared with a device while both run, such as a descriptor ring.

```nan-api
// module: hw

pub open linear struct Dma[T: AnyBits] {
} Dma

pub fn Dma.of(take contents: mem::Box[T]) -> Dma[T]

pub fn Dma.bus_addr(self) -> BusAddr limits notrap, terminates, noalloc

pub proj fn Dma.contents(self) -> T

pub proj fn Dma.contents_mut(mut self) -> T

pub fn Dma.into_box(take self) -> mem::Box[T]

pub open linear struct DmaShared[T: AnyBits] {
} DmaShared

pub fn DmaShared.of(take contents: mem::Box[T]) -> DmaShared[T]

pub fn DmaShared.bus_addr(self) -> BusAddr limits notrap, terminates, noalloc

pub fn DmaShared.into_box(take self) -> mem::Box[T]

pub fn dma_fence()
```

- Every operation of this section is a device operation (`authority.ungranted`), has the device fact, terminates and allocates nothing.
- A buffer's memory is its box's contents, which never move. On the machine targets its bus address is the physical address that the address of the contents translates to when `of` runs, and a device reaches the size of `T` bytes from it, which are contiguous in physical memory ([physical memory](24-kernel.md#physical-memory)). On the host targets its bus address is the address of the contents, and only [simulated devices](22-testing.md#simulated-capabilities) reach it.
- The T of `hw::Dma[T]` and `hw::DmaShared[T]` has a [fixed layout](21-interpreter.md#interpreter-memory); another T is an error (`hw.layout-unfixed`), reported where the type is written.
- The contents of an `hw::DmaShared[T]` are reached only through its **DMA access calls**, whose field operand is the member path `T.f` of a field f of T, written directly, where T is a bytes layout. Another operand, or a call not matching its field's row, is an error (`hw.dma-operand`).

| Call | Field | Result |
| --- | --- | --- |
| `mut d.read_acquire(T.f)` | not an array | the field's type |
| `mut d.write_release(T.f, value: v)` | not an array | `Unit` |
| `mut d.read_acquire_at(T.f, index: i)` | an array | the element type |
| `mut d.write_release_at(T.f, index: i, value: v)` | an array | `Unit` |

- A DMA access call performs one access for each integer scalar and integer layout of the field or element, in ascending address order, each of its width. An index not below the array's length traps (`trap.bounds`).
- No memory access of the task that follows a `read_acquire` in program order takes effect before it, and every memory access that precedes a `write_release` takes effect before it. `hw::dma_fence()` makes every earlier memory access of the task take effect before every later one.
- Accesses to DMA buffer memory and volatile accesses take effect in program order for devices: the implementation moves no access to DMA buffer memory across a volatile access.
- A [completion operation](17-suspension-io.md#completion-io) whose buffer type contains an `hw::Dma[T]` other than inside an `Untrusted` is an error (`hw.dma-completion`), so a buffer a device may have written returns Untrusted ([sources](15-untrusted.md#sources)); `into_cleared` makes it reusable ([content-blind operations](15-untrusted.md#content-blind-operations)).

```nan
// file: kernel/package.nan
//! A block device's queues.

package kernel {
    language 1
    targets x86_64-bare
    allow device in kernel
} kernel

// file: kernel/kernel.nan
//! The used ring and sector buffers of a block device.

pub layout UsedEntry: bytes 8 le {
    id: u32 at 0
    length: u32 at 4
} UsedEntry

pub layout UsedRing: bytes 516 le {
    flags: u16 at 0
    index: u16 at 2
    entries: Array[UsedEntry, 64] at 4
} UsedRing

pub layout Sector: bytes 512 le {
    data: Array[u8, 512] at 0
} Sector

pub fn next_used(mut ring: hw::DmaShared[UsedRing], seen: u16) -> Option[UsedEntry] {
    let published = mut ring.read_acquire(UsedRing.index)
    if published == seen {
        return None
    }
    let slot = usize.widen(seen % 64)
    return Some(mut ring.read_acquire_at(UsedRing.entries, index: slot))
} next_used

pub fn sector_buffer(heap: mem::Heap) -> hw::Dma[Sector] {
    let contents = heap.boxed(Sector(data: Array[u8, 512].filled(0)))
    return hw::Dma.of(take contents)
} sector_buffer
```

```nan
// file: kernel/package.nan
//! A device's status word.

package kernel {
    language 1
    targets x86_64-bare
    allow device in kernel
} kernel

// file: kernel/kernel.nan
//! Clears a device's status word.

pub layout Status: bytes 4 le {
    pending: u32 at 0
} Status

pub fn clear(mut status: hw::DmaShared[Status]) {
    mut status.write_release(Status.pending) // error: hw.dma-operand
} clear
```

```nan
// file: kernel/package.nan
//! A block device's sector reads.

package kernel {
    language 1
    targets x86_64-bare
} kernel

// file: kernel/kernel.nan
//! Returns sector buffers once the device signals completion.

pub layout Sector: bytes 512 le {
    data: Array[u8, 512] at 0
} Sector

pub fn read_sector(take buf: Untrusted[hw::Dma[Sector]], done: mem::Shared[hw::Signal[u32]]) -> task::Completion[Untrusted[hw::Dma[Sector]], u32, task::Cancelled] fails task::Cancelled uses wait {
    let outcome = attempt await done.contents().wait()
    return task::Completion(buf: take buf, outcome: take outcome)
} read_sector
```

```nan
// file: kernel/package.nan
//! A block device's sector reads.

package kernel {
    language 1
    targets x86_64-bare
} kernel

// file: kernel/kernel.nan
//! Returns sector buffers once the device signals completion.

pub layout Sector: bytes 512 le {
    data: Array[u8, 512] at 0
} Sector

pub fn read_sector(take buf: hw::Dma[Sector], done: mem::Shared[hw::Signal[u32]]) -> task::Completion[hw::Dma[Sector], u32, task::Cancelled] fails task::Cancelled uses wait { // error: hw.dma-completion
    let outcome = attempt await done.contents().wait()
    return task::Completion(buf: take buf, outcome: take outcome)
} read_sector
```

```nan
// file: kernel/package.nan
//! Sensor buffers for a bare kernel.

package kernel {
    language 1
    targets x86_64-bare
    allow device in kernel
} kernel

// file: kernel/kernel.nan
//! Recovers sensor readings from DMA buffers.

pub closed struct Reading {
    pub low: u32
    pub high: u32
} Reading

pub fn reading_box(take buffer: hw::Dma[Reading]) -> mem::Box[Reading] { // error: hw.layout-unfixed
    return take buffer.into_box()
} reading_box
```

## Privileged intrinsics

A **privileged intrinsic** is a method of `hw::Ring0` performing one privileged operation on the running CPU or, for the cross-CPU intrinsics, on every CPU. `hw::Ring0` is a primitive capability; [Kernel execution model](24-kernel.md#anchor) states where a value comes from. Calling a privileged intrinsic is a `priv` operation (`authority.ungranted`). The privileged intrinsics of every machine target include these.

```nan-api
// module: hw

pub open linear struct Ring0 {
} Ring0

pub fn Ring0.invalidate_page(self, page: VirtAddr)

pub fn Ring0.invalidate_all(self)

pub fn Ring0.wait_for_interrupt(self, t: IrqsDisabled)

pub fn Ring0.signal_cpu(self, cpu: u32)

pub fn Ring0.invalidate_everywhere(self, scope: TlbScope) fails task::Cancelled uses wait

pub closed enum TlbScope derives Copy, Eq {
    Page(VirtAddr)
    All
} TlbScope
```

- `invalidate_page` removes the running CPU's cached translations of the page holding `page` under its current PCID or ASID, and `invalidate_all` every cached translation of the running CPU, global ones and those of every PCID or ASID included.
- `wait_for_interrupt` halts the running CPU until an interrupt arrives, enabling maskable interrupts while it halts ([interrupt state](24-kernel.md#interrupt-state)). It returns at once inside an interrupt fn and before the [anchor](24-kernel.md#anchor) is installed.
- `signal_cpu` and `invalidate_everywhere` are the **cross-CPU intrinsics**, which [cross-CPU operations](24-kernel.md#cross-cpu-operations) defines.
- Every privileged intrinsic has the priv fact, never allocates and never traps, and terminates unless its head states `uses wait`. An intrinsic removing cached translations may remove others too.

The privileged intrinsics of `x86_64-uefi` and `x86_64-bare` are exactly those of every machine target and these.

```nan-api
// module: hw

pub fn Ring0.load_idt(self, take idt: mem::Box[Idt]) -> IdtInstalled

pub fn Ring0.write_cr3(self, take root: PageTableRoot) -> Option[PageTableRoot]

pub fn Ring0.write_cr3_tagged(self, take root: PageTableRoot, pcid: u16, keep: bool) -> Option[PageTableRoot]
    requires pcid < 4_096

pub fn Ring0.invalidate_pcid(self, pcid: u16)
    requires pcid < 4_096

pub fn Ring0.invalidate_pcid_page(self, pcid: u16, page: VirtAddr)
    requires pcid < 4_096

pub fn Ring0.read_msr(self, msr: u32) -> u64

pub fn Ring0.write_msr(self, msr: u32, value: u64)

pub fn Ring0.cpuid(self, leaf: u32, subleaf: u32) -> CpuidRegs

pub fn Ring0.read_tsc(self) -> u64

pub fn Ring0.read_tscp(self) -> TscRead

pub fn Ring0.monitor_wait(self, t: IrqsDisabled, word: core::atomic::AtomicU64, seen: u64, hint: u8)

pub fn Ring0.read_xcr0(self) -> u64

pub fn Ring0.write_xcr0(self, value: u64) -> bool

pub fn Ring0.hardware_random(self) -> Option[u64]

pub fn Ring0.hardware_seed(self) -> Option[u64]

pub fn Ring0.enable_protection(self, feature: Protection) -> bool

pub fn Ring0.clear_cpu_buffers(self)

pub closed struct CpuidRegs derives Copy, Eq {
    pub eax: u32
    pub ebx: u32
    pub ecx: u32
    pub edx: u32
} CpuidRegs

pub closed struct TscRead derives Copy, Eq {
    pub count: u64
    pub cpu: u32
} TscRead

pub closed enum Protection derives Copy, Eq {
    Smep
    Smap
    Umip
    Pcid
} Protection

pub open struct Idt {
} Idt

pub fn Idt.of_vectors() -> Idt limits pure, notrap, terminates
```

- `hw::Idt` is an interrupt descriptor table whose gate n is an interrupt gate of privilege level 0 entering the [machine seam](#machine-seams) of vector n with the `ist` of n's `hw::vectors` entry, or 0 without one. `Idt.of_vectors()` builds it.
- `load_idt` loads the table into the running CPU's interrupt descriptor table register.
- `write_cr3` makes `root` the running CPU's page-table root and returns `Some` of a `hw::PageTableRoot` owning the frame of the root it replaces, or `None` when it replaces the [entry root](24-kernel.md#physical-memory). It is a trusted primitive whose soundness condition is that `root` maps the running code, its stack and every live value at their current addresses, and keeps the image, the memory it reserves and every live frame and region mapped at their physical addresses; that no other `hw::PageTableRoot` owns the frame of the replaced root; and that until a later `write_cr3` on that CPU returns `root`, no value that safe code reaches owns or views memory of a page table that `root` reaches, other than `root` itself.
- `write_cr3_tagged` installs `root` as `write_cr3` does, as a later `write_cr3` or `write_cr3_tagged` returns it, but tags the translations the CPU caches from it with PCID `pcid`. It keeps the translations cached under other PCIDs, and those under `pcid` too when `keep` is true. While the CPU has PCIDs disabled it ignores `pcid` and `keep`. It is a trusted primitive whose soundness condition is that of `write_cr3`, and that each translation cached under `pcid` that it keeps maps as `root` does.
- `invalidate_pcid_page` removes the running CPU's cached translations of the page holding `page` under `pcid`, and `invalidate_pcid` every one under `pcid`; while the CPU has PCIDs disabled, PCID 0 tags every translation.
- `read_msr` and `write_msr` read and write the model-specific register `msr`. Both are trusted primitives, with the soundness condition that `msr` exists and that a write leaves every translation, stack and support routine state that code relies on intact. The indirect branch prediction barrier is `write_msr(0x49, value: 1)`.
- `cpuid` gives the four registers the `cpuid` instruction leaves for `leaf` in `eax` and `subleaf` in `ecx`.
- `read_tsc` reads the time-stamp counter once every earlier instruction has completed. `read_tscp` reads it as `count` together with `cpu`, the number of the CPU that read it, which the runtime keeps in each CPU's `TSC_AUX` for `rdtscp`; on a CPU without `rdtscp` it reads both with maskable interrupts disabled.
- `monitor_wait` arms `monitor` on `word` and, when `word` still holds `seen`, waits in `mwait` with `hint` as its C-state hint until `word` may have changed, an interrupt arrives, or for no reason; maskable interrupts stay disabled and an arriving one ends the wait. It returns at once on a CPU whose `mwait` cannot end on an interrupt while they are disabled.
- `read_xcr0` gives `XCR0`, or 0 while the CPU has XSAVE disabled. `write_xcr0` writes `value` to `XCR0` and returns true, or changes nothing and returns false while the CPU has XSAVE disabled or when `value` enables a state component the CPU lacks, leaves x87 state disabled, enables AVX state without SSE state, or enables a component other than these three, whose state the [vector seams](#machine-seams) do not save.
- `hardware_random` gives a value of `rdrand` and `hardware_seed` one of `rdseed`, trying up to 10 times, or `None` when the CPU lacks the instruction or every try fails.
- `enable_protection` enables `feature` on the running CPU and returns true, or returns false when the CPU lacks it. `Smep` keeps the CPU from executing pages mapped for user mode, `Smap` from accessing them other than through `UserAddr.copy_in`, `Umip` keeps user mode from reading descriptor-table registers, and `Pcid` enables the PCIDs that `write_cr3_tagged` tags. No intrinsic disables one again.
- `clear_cpu_buffers` overwrites the CPU buffers that microarchitectural data sampling reads, through `verw`.

The privileged intrinsics of `aarch64-uefi` and `aarch64-bare` are exactly those of every machine target and these.

```nan-api
// module: hw

pub fn Ring0.write_ttbr0(self, take root: PageTableRoot) -> Option[PageTableRoot]

pub fn Ring0.write_ttbr0_tagged(self, take root: PageTableRoot, asid: u8) -> Option[PageTableRoot]

pub fn Ring0.invalidate_asid(self, asid: u8)

pub fn Ring0.invalidate_asid_page(self, asid: u8, page: VirtAddr)

pub fn Ring0.read_sysreg(self, register: SysReg) -> u64

pub fn Ring0.write_sysreg(self, register: SysReg, value: u64)
    requires register.is_writable()

pub fn Ring0.wait_for_event(self, t: IrqsDisabled, word: core::atomic::AtomicU64, seen: u64)

pub fn Ring0.clean_data_cache(self, region: Region)

pub fn Ring0.clean_invalidate_data_cache(self, region: Region)

pub fn Ring0.invalidate_data_cache(self, region: Region)

pub fn Ring0.sync_instruction_cache(self, region: Region)

pub fn Ring0.firmware_call(self, function: u32, a: u64, b: u64, c: u64) -> u64
pub closed enum SysReg derives Copy, Eq {
    SctlrEl1
    TcrEl1
    MairEl1
    Ttbr1El1
    ContextidrEl1
    CpacrEl1
    CntkctlEl1
    TpidrEl0
    TpidrroEl0
    CntpCtlEl0
    CntpCvalEl0
    CntpTvalEl0
    CntvCtlEl0
    CntvCvalEl0
    CntvTvalEl0
    MidrEl1
    MpidrEl1
    CntfrqEl0
    CntpctEl0
    CntvctEl0
    CtrEl0
    IdAa64pfr0El1
    IdAa64isar0El1
    IdAa64mmfr0El1
} SysReg

pub fn SysReg.is_writable(self) -> bool limits pure, notrap, terminates
```

- `write_ttbr0` makes `root` the root of the running CPU's translations of the addresses below `0x0001_0000_0000_0000`, held in `TTBR0_EL1`, invalidates the CPU's cached translations and returns as `write_cr3` does, with the same soundness condition.
- `write_ttbr0_tagged` installs `root` as `write_ttbr0` does, as a later `write_ttbr0` or `write_ttbr0_tagged` returns it, but tags the translations the CPU caches from it with ASID `asid` and invalidates none. It is a trusted primitive whose soundness condition is that of `write_ttbr0`, and that each translation cached under `asid` maps as `root` does.
- `invalidate_asid_page` removes the running CPU's cached translations of the page holding `page` under `asid`, and `invalidate_asid` every one under `asid`.
- `read_sysreg` and `write_sysreg` read and write the system register that `register` names, `SctlrEl1` naming `SCTLR_EL1`, `CntpCtlEl0` naming `CNTP_CTL_EL0` and so on. `register.is_writable()` is true exactly for the registers from `SctlrEl1` to `CntvTvalEl0`. `write_sysreg` is a trusted primitive whose soundness condition is that the write leaves every translation, stack and support routine state that code relies on intact.
- `wait_for_event` reads `word` with an exclusive load and, when it holds `seen`, waits in `wfe` until `word` may have changed, an event arrives, or for no reason; maskable interrupts stay disabled.
- `clean_data_cache` writes the data cache lines holding `region` back to memory, `clean_invalidate_data_cache` also removes them from the caches, and `invalidate_data_cache` removes them without writing them back; `sync_instruction_cache` makes instruction fetches on every CPU see the bytes of `region`. Each returns once its maintenance is complete for every observer, devices included. `invalidate_data_cache` is a trusted primitive whose soundness condition is that the lines hold no write that code relies on and that memory lacks.
- `firmware_call` calls the firmware's PSCI interface through the descriptor's conduit with `function` in `w0` and `a`, `b` and `c` in `x1` to `x3`, and gives `x0`. It is a trusted primitive whose soundness condition is that the call leaves every CPU, translation, stack and support routine state that code relies on intact.

Each privileged intrinsic of one architecture's list, and `hw::Idt.of_vectors`, belongs to that architecture. A call of one in code checked for a target of another architecture, or within the reach of a final build for one ([environments](27-program-model.md#environments)), is an error (`hw.intrinsic-target`).

An **install token** is a linear value that an intrinsic returns when the hardware keeps referencing its argument. Owning the token keeps the argument alive and unmoved.

```nan-api
// module: hw

pub open linear struct IdtInstalled {
} IdtInstalled

pub open linear struct PageTableRoot {
} PageTableRoot

pub fn PageTableRoot.of(take frame: PhysFrame[4_096]) -> PageTableRoot

pub fn PageTableRoot.into_frame(take self) -> PhysFrame[4_096]
```

- `hw::IdtInstalled` holds the box `load_idt` took. No std operation releases it, so the table stays allocated while the machine runs; a kernel moves the token into its [anchor](24-kernel.md#anchor).
- `hw::PageTableRoot` owns the [physical frame](24-kernel.md#physical-memory) of a top-level page table and is a primitive capability. The root that a root-installing intrinsic takes is held by the CPU until a later call of one installing a root on that CPU returns it.

```nan
// file: kernel/package.nan
//! CPU tables for a bare kernel.

package kernel {
    language 1
    targets x86_64-bare
    allow priv in kernel
    allow trusted in kernel
} kernel

// file: kernel/kernel.nan
//! Interrupt vectors and address-space switching.

pub fn install_vectors(cpu: hw::Ring0, heap: mem::Heap) -> hw::IdtInstalled {
    let table = heap.boxed(hw::Idt.of_vectors())
    return cpu.load_idt(take table)
} install_vectors

pub fn activate(take root: hw::PageTableRoot, cpu: hw::Ring0) -> Option[hw::PageTableRoot] {
    return trusted "root maps the running kernel image and the current stack" { cpu.write_cr3(take root) }
} activate
```

```nan
// file: kernel/package.nan
//! Address spaces for an AArch64 bare kernel.

package kernel {
    language 1
    targets aarch64-bare
    allow priv in kernel
    allow trusted in kernel
} kernel

// file: kernel/kernel.nan
//! Address-space switching and the timer's frequency.

pub fn activate(take root: hw::PageTableRoot, cpu: hw::Ring0) -> Option[hw::PageTableRoot] {
    return trusted "root maps the running kernel image and the current stack" { cpu.write_ttbr0(take root) }
} activate

pub fn timer_hertz(cpu: hw::Ring0) -> u64 {
    return cpu.read_sysreg(hw::SysReg.CntfrqEl0)
} timer_hertz
```

```nan
// file: kernel/package.nan
//! Address spaces for an AArch64 bare kernel.

package kernel {
    language 1
    targets aarch64-bare
    allow priv in kernel
    allow trusted in kernel
} kernel

// file: kernel/kernel.nan
//! Address-space switching.

pub fn activate(take root: hw::PageTableRoot, cpu: hw::Ring0) -> Option[hw::PageTableRoot] {
    return trusted "root maps the running kernel image and the current stack" { cpu.write_cr3(take root) } // error: hw.intrinsic-target
} activate
```

```nan
// file: kcpu/package.nan
//! CPU identification in a bare kernel.

package kcpu {
    language 1
    targets x86_64-bare
    allow priv in kcpu
} kcpu

// file: kcpu/kcpu.nan
//! Reads the highest CPUID leaf and switches address spaces keeping their translations.

pub fn max_leaf(cpu: hw::Ring0) -> u32 {
    return cpu.cpuid(0, subleaf: 0).eax
} max_leaf

pub fn switch_to(take root: hw::PageTableRoot, pcid: u16, cpu: hw::Ring0) -> Option[hw::PageTableRoot]
    requires pcid < 4_096
{
    return cpu.write_cr3_tagged(take root, pcid, keep: true) // error: trusted.primitive-outside
} switch_to
```

```nan
// file: kcpu/package.nan
//! CPU identification in an AArch64 bare kernel.

package kcpu {
    language 1
    targets aarch64-bare
    allow priv in kcpu
} kcpu

// file: kcpu/kcpu.nan
//! Reads the highest CPUID leaf.

pub fn max_leaf(cpu: hw::Ring0) -> u32 {
    return cpu.cpuid(0, subleaf: 0).eax // error: hw.intrinsic-target
} max_leaf
```

## Machine seams

A **machine seam** is a compiler-generated stub at a fixed hardware entry, calling an interrupt fn or landing fn. Each [descriptor](27-program-model.md#descriptors) fixes its seams; they are exactly:

| Seam | Targets | Entered by | Calls |
| --- | --- | --- | --- |
| program entry | every concrete target | the platform starting the image | the descriptor's entry landing fn, then the [entry function](27-program-model.md#entry) with the root grant it returns |
| vector n, n from 0 to 255 | the machine targets | on x86_64, an interrupt or exception through gate n; on AArch64, an exception of vector n's kind, or interrupt n of the interrupt controller | the [interrupt fn](24-kernel.md#interrupt-functions) or landing fn bound to n |
| syscall | the bare targets | the `syscall` instruction on x86_64 and the `svc` instruction on AArch64, in user mode | the landing fn of `hw::syscall_entry` |

- [Platform registries](25-registries.md#platform-registries) and `hw::IrqShared.install` ([locks](24-kernel.md#locks)) bind the vector and syscall seams. [Interrupt functions](24-kernel.md#interrupt-functions) and [kernel halts](24-kernel.md#kernel-halts) state what a vector seam without a binding does, and platform registries what the syscall seam does without one.
- A seam saves the interrupted state, switches to its context's stack, passes its function the arguments that function's kind receives, and restores the state that function's result selects. [Kernel execution model](24-kernel.md) fixes the saved state and stacks.

### Compiler stubs

A **compiler stub** is machine code that the implementation emits from its descriptor's fixed list: the machine seams, and one **out-of-line stub** for each privileged intrinsic, `hw::UserAddr.copy_in`, each volatile access kind and width (memory-mapped reads and writes of 1, 2, 4 and 8 bytes; on x86_64, port reads and writes of 1, 2 and 4 bytes), each width of a DMA access call and `hw::dma_fence`.

- Lowering performs each of these operations as a call of its out-of-line stub, never inlined. No memory access moves across that call.
- The list of a descriptor changes only with the [language version](37-evolution.md#language-versions). The machine code of an image is exactly its lowered code, its compiler stubs and its [support routines](27-program-model.md#runtime-support).

## Landing functions

A **landing fn** is a function with the `landing` modifier, entered from a machine seam with raw values.

- Declaring a landing fn is a `priv` operation (`authority.ungranted`).
- Its parameters are read parameters whose types are integer scalars, `hw::Ptr[T]`, AnyBits layouts, `Untrusted[I]` with I an integer scalar ([sources](15-untrusted.md#sources)), and arrays of these. Its result is `Unit`, `Never`, an integer scalar, `hw::Ptr[T]` or an AnyBits layout, or, for an entry landing fn, the target's [root grant](14-authority.md#root-grants). It has no generic parameters, `fails` or `uses wait`. Any other head is an error (`hw.landing-signature`).
- Only its seam calls it. A call of a landing fn, and a use of one as a function value other than in a contribution to a [platform registry](25-registries.md#platform-registries), are errors (`hw.landing-called`).
- [Static bounds](17-suspension-io.md#static-bounds) govern its stack. Each landing fn is a trust escape.

`hw::mint_root()` is an intrinsic whose type is the root grant of the target being checked. It is legal only in the entry landing fn of the target's [platform package](28-stdlib.md#platform-packages), which the descriptor names, at most once there and outside every loop and closure (`hw.mint-root-misplaced`). The program entry seam enters that landing fn once per run, so each run mints exactly one root grant.

```nan
// file: kernel/package.nan
//! System call entry for a bare kernel.

package kernel {
    language 1
    targets x86_64-bare
    allow priv in kernel
    allow validate in kernel
} kernel

// file: kernel/kernel.nan
//! Decodes system call numbers.

validator fn call_word(raw: Untrusted[u64]) -> u64 {
    return raw.inspect()
} call_word

pub landing fn enter_syscall(number: Untrusted[u64], args: Array[Untrusted[u64], 6]) -> u64 limits stack 4_096 {
    if call_word(number) == 0 {
        return call_word(args[0])
    }
    return 0
} enter_syscall
```

```nan
// file: kernel/package.nan
//! System call entry for a bare kernel.

package kernel {
    language 1
    targets x86_64-bare
    allow priv in kernel
    allow validate in kernel
} kernel

// file: kernel/kernel.nan
//! Decodes system call numbers.

validator fn call_word(raw: Untrusted[u64]) -> u64 {
    return raw.inspect()
} call_word

pub landing fn enter_syscall(number: Untrusted[u64], args: Array[Untrusted[u64], 6]) -> u64 limits stack 4_096 {
    if call_word(number) == 0 {
        return call_word(args[0])
    }
    return 0
} enter_syscall

pub fn echo(number: Untrusted[u64], args: Array[Untrusted[u64], 6]) -> u64 {
    return enter_syscall(number, args) // error: hw.landing-called
} echo
```

## Foreign functions

A **foreign fn** is a function with the `foreign` modifier declaring an external symbol with a calling convention and an AnyBits head. Its head ends with a convention clause and it has no body ([heads](02-items.md#heads)).

```ebnf
convention_clause = 'convention' TEXT ;
```

- Declaring a foreign fn is a `trusted` operation (`authority.ungranted`), and each declaration is a trust escape.
- Its parameters are read parameters, and its parameter and result types are integer and float scalars, `hw::Ptr[T]` and AnyBits layouts; its result may also be `Unit` or `Never`. Its name is one identifier, and it has no generic parameters or `fails`. Any other head is an error (`trusted.foreign-signature`). [Effects, digests and limits](10-effects-digests.md#foreign-functions-and-trusted-code) gives its waiting and digest rules.
- Its symbol is its name. A call of a foreign fn, or through a foreign function type, passes and returns an integer or float scalar as the convention passes the C type of that width and signedness, a pointer as its address, an integer layout as its bits value, and a bytes layout as a structure of N bytes with alignment 1.
- A final build of a host target's image resolves the symbols of the foreign fns it reaches through its linker ([images](27-program-model.md#images)). A final build of a machine target's image reaching a foreign fn is an error (`trusted.foreign-unresolved`); on those targets, foreign function types describe the firmware's entries.

### Calling conventions

| Convention | Meaning | Targets |
| --- | --- | --- |
| `"sysv"` | the calling convention of the System V ABI for the target's architecture: the AMD64 one on x86_64 and the Procedure Call Standard for the Arm 64-bit Architecture on AArch64 | `x86_64-host`, `aarch64-host` |
| `"win64"` | the Microsoft x64 calling convention | `x86_64-uefi` |
| `"aapcs64"` | the Procedure Call Standard for the Arm 64-bit Architecture, with `x18` reserved for the platform | `aarch64-uefi` |

- Any other convention text is an error (`trusted.convention-unknown`).
- A convention clause, on a foreign fn or a [function type](10-effects-digests.md#function-types), in a package declaring a target that the convention's row does not list, `any` included, is an error (`trusted.convention-target`).

```nan
// file: procinfo/package.nan
//! Process facts on the host.

package procinfo {
    language 1
    targets x86_64-host
    allow trusted in procinfo
} procinfo

// file: procinfo/procinfo.nan
//! Reads the process id and the last error number from the C library.

foreign fn getpid() -> i32 limits notrap, terminates, noalloc convention "sysv"

foreign fn __errno_location() -> hw::Ptr[i32] limits notrap, terminates, noalloc convention "sysv"

pub fn process_id() -> i32 {
    return getpid()
} process_id

pub fn last_error() -> i32 {
    return trusted "the C library gives the calling thread's error number, which lives as long as the thread" { __errno_location().read() }
} last_error
```

```nan
// file: procinfo/package.nan
//! Process facts on the host.

package procinfo {
    language 1
    targets x86_64-host
    allow trusted in procinfo
} procinfo

// file: procinfo/procinfo.nan
//! Reads from the C library.

foreign fn read(fd: i32, mut buf: SliceMut[u8]) -> isize convention "sysv" // error: trusted.foreign-signature

pub fn read_input(mut buf: SliceMut[u8]) -> isize {
    return read(0, mut buf)
} read_input
```

```nan
// file: procinfo/package.nan
//! Process facts on both host architectures.

package procinfo {
    language 1
    targets aarch64-host, x86_64-host
    allow trusted in procinfo
} procinfo

// file: procinfo/procinfo.nan
//! Reads the process id from the C library.

foreign fn getpid() -> i32 limits notrap, terminates, noalloc convention "sysv"

pub fn process_id() -> i32 {
    return getpid()
} process_id
```

```nan
// file: procinfo/package.nan
//! Process facts on AArch64 hosts.

package procinfo {
    language 1
    targets aarch64-host
    allow trusted in procinfo
} procinfo

// file: procinfo/procinfo.nan
//! Reads the process id from the C library.

foreign fn getpid() -> i32 limits notrap, terminates, noalloc convention "aapcs64" // error: trusted.convention-target

pub fn process_id() -> i32 {
    return getpid()
} process_id
```

## Memory model

The **memory model** is the set of rules for memory that several tasks and interrupt fn runs reach.

- An access **happens before** another when both belong to one task or interrupt fn run and the first precedes the second in [evaluation order](06-statements-expressions.md#evaluation-order); when an operation the first precedes synchronises with an operation the second follows; or through a chain of these. An operation of [Structured concurrency](16-concurrency.md) or [Kernel execution model](24-kernel.md) that passes a value, a lock or a completion from one task or interrupt fn run to another synchronises with the operation receiving it, and atomics synchronise as [trusted atomics](#trusted-atomics) state, and count changes of shared values and cown handles as [shared values](12-memory.md#shared-values) and [cowns](16-concurrency.md#cowns) state.
- A **data race** is two accesses to one byte by different tasks or interrupt fn runs, at least one a write and at least one not atomic, neither of which happens before the other.
- Safe code has no data race: [exclusivity](07-ownership.md#exclusivity), [Send](05-properties.md#send), [lending](16-concurrency.md#lending) and the atomic operations of types with [interior mutability](12-memory.md#interior-mutability) exclude them.
- Safe code is sequentially consistent: every execution without undefined behaviour is, as safe code observes it, one interleaving of the operations of its tasks and interrupt fn runs, each in evaluation order.

### Ptr rules

The **Ptr rules** govern every access and offset trusted code makes through `hw::Ptr`:

- provenance: a pointer made from a place or view has the allocation holding it as provenance, and, when made through a `mut` access, that access; an access reaches only the live allocation of its provenance, and a pointer without provenance reaches only memory the premise names;
- bounds: `offset` stays within its allocation or just past its end, and an access lies inside it;
- alignment: an access's address is a multiple of its type's alignment;
- read-only: no write reaches a const, a literal, or memory the pointer reaches through a read access, outside a value with interior mutability;
- initialisation: a read covers only initialised bytes;
- validity: the bytes a read covers form a value of its type, its tags, niches and predicates included;
- layout: an access covers a value of a type other than scalars, `hw::Ptr`, layouts and arrays of these only whole and as that type;
- exclusivity: no access reaches a place passed by `mut` to a running call except through a pointer made through that access;
- races: no access is part of a data race.

### Trusted atomics

```nan-api
// module: hw

pub closed enum Order derives Copy, Eq {
    Relaxed
    Acquire
    Release
    AcqRel
    SeqCst
} Order

pub fn Ptr.atomic_load(self, order: Order) -> T
    requires self.is_aligned()
    requires order != Order.Release and order != Order.AcqRel

pub fn Ptr.atomic_store(self, value: T, order: Order)
    requires self.is_aligned()
    requires order != Order.Acquire and order != Order.AcqRel

pub fn Ptr.atomic_swap(self, value: T, order: Order) -> T
    requires self.is_aligned()

pub fn Ptr.atomic_compare_swap(self, expected: T, replacement: T, order: Order) -> Result[T, T]
    requires self.is_aligned()

pub fn Ptr.atomic_wrapping_add(self, amount: T, order: Order) -> T
    requires self.is_aligned()

pub fn fence(order: Order)
```

- The `atomic_` methods apply to `u8`, `u16`, `u32`, `u64` and `usize`; on another T they are errors (`hw.atomic-type`).
- The `atomic_` methods and `hw::fence` terminate, allocate nothing and have the sync fact ([digests](10-effects-digests.md#digests)); their only trap sites are their structured requires.
- They and `hw::fence` behave as the C++20 atomic operations and fences with the matching `std::memory_order`, which excludes `memory_order_consume`. `atomic_compare_swap` is a strong compare-exchange; it uses `order` on success and, on failure, `Relaxed` for `Release`, `Acquire` for `AcqRel` and `order` otherwise. The atomics of `core::atomic` are `SeqCst` operations of this model.
- Two atomic accesses to overlapping bytes that differ in address or width are a data race unless one happens before the other.
- `atomic_compare_swap` returns `Ok` with the old value when it equals `expected`, after writing `replacement`, and `Err` with the old value otherwise. `atomic_wrapping_add` adds modulo 2^n, n the width of T in bits, and returns the old value.

```nan
// file: ring/package.nan
//! Shared flags for rings.

package ring {
    language 1
    targets any
    allow trusted in ring
} ring

// file: ring/ring.nan
//! A claim flag shared through a pointer.

pub fn claim_flag(flag: hw::Ptr[u32]) -> bool {
    let previous = trusted "flag addresses a live u32 that every task reaches only atomically" { flag.atomic_swap(1, order: hw::Order.Acquire) }
    return previous == 0
} claim_flag

pub fn release_flag(flag: hw::Ptr[u32]) {
    trusted "flag addresses a live u32 that every task reaches only atomically" {
        flag.atomic_store(0, order: hw::Order.Release)
    }
} release_flag
```

## Hardware types

The types of this table are [base types](05-properties.md#structural-properties) with these properties. The primitive capabilities among them are `hw::Mmio[R]`, `hw::Ports[R]`, `hw::Dma[T]`, `hw::DmaShared[T]`, `hw::Ring0` and `hw::PageTableRoot`.

| Type | linear | view | Copy | Send | Inert | AnyBits |
| --- | --- | --- | --- | --- | --- | --- |
| `hw::Ptr[T]`, `hw::UserAddr[T]` | no | no | yes | yes | yes | yes |
| `hw::Register[R, T]` | no | no | yes | yes | yes | no |
| `hw::Mmio[R]`, `hw::Ports[R]`, `hw::Dma[T]`, `hw::DmaShared[T]`, `hw::Ring0`, `hw::PageTableRoot` | yes | no | no | yes | no | no |
| `hw::IdtInstalled` | yes | no | no | yes | no | no |
| `hw::Idt` | no | no | no | yes | yes | no |

## Diagnostics

| Code | Class | Rule |
| --- | --- | --- |
| `hw.atomic-type` | error | [Trusted atomics](#trusted-atomics) |
| `hw.dma-completion` | error | [DMA](#dma) |
| `hw.dma-operand` | error | [DMA](#dma) |
| `hw.intrinsic-target` | error | [Privileged intrinsics](#privileged-intrinsics) |
| `hw.landing-called` | error | [Landing functions](#landing-functions) |
| `hw.landing-signature` | error | [Landing functions](#landing-functions) |
| `hw.layout-unfixed` | error | [Address types](#address-types), [DMA](#dma) |
| `hw.mint-root-misplaced` | error | [Landing functions](#landing-functions) |
| `layout.endianness` | error | [Layouts](#layouts) |
| `layout.field-form` | error | [Layouts](#layouts) |
| `layout.field-type` | error | [Layouts](#layouts) |
| `layout.literal` | error | [Layouts](#layouts) |
| `layout.out-of-range` | error | [Layouts](#layouts) |
| `layout.overlap` | error | [Layouts](#layouts) |
| `layout.repr` | error | [Layouts](#layouts) |
| `layout.tag-value` | error | [Layouts](#layouts) |
| `layout.uncovered` | error | [Layouts](#layouts) |
| `register.array-access` | error | [Register windows](#register-windows), [Sim constructors](22-testing.md#sim-constructors) |
| `register.bank-reserved` | error | [Registers](#registers) |
| `register.literal` | error | [Registers](#registers) |
| `register.misaligned` | error | [Registers](#registers) |
| `register.not-readable` | error | [Register windows](#register-windows) |
| `register.not-writable` | error | [Register windows](#register-windows) |
| `register.operand` | error | [Register windows](#register-windows), [Sim constructors](22-testing.md#sim-constructors) |
| `register.overlap` | error | [Registers](#registers) |
| `register.port-target` | error | [Register windows](#register-windows) |
| `register.port-width` | error | [Register windows](#register-windows) |
| `register.stride` | error | [Registers](#registers) |
| `register.type` | error | [Registers](#registers) |
| `register.window-type` | error | [Registers](#registers) |
| `trusted.block-value` | error | [Trusted code](#trusted-code) |
| `trusted.convention-target` | error | [Calling conventions](#calling-conventions) |
| `trusted.convention-unknown` | error | [Calling conventions](#calling-conventions) |
| `trusted.empty-premise` | error | [Trusted code](#trusted-code) |
| `trusted.foreign-signature` | error | [Foreign functions](#foreign-functions) |
| `trusted.foreign-unresolved` | error | [Foreign functions](#foreign-functions) |
| `trusted.nested` | error | [Trusted code](#trusted-code) |
| `trusted.primitive-outside` | error | [Trusted primitives](#trusted-primitives) |
