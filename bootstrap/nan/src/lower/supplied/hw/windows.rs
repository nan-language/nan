//! Register windows, the simulated register blocks of `hw::sim::Registers`, and DMA buffers. A
//! window reaches a simulated block or, on a machine target, a device's port or physical address
//! through compiler stubs; on a machine target each part of a DMA access is a compiler stub too,
//! and a buffer in an I/O domain's space moves into pages of its own that the domain maps.

use cranelift_codegen::ir::{InstBuilder, Type, Value, condcodes::IntCC, types};
use cranelift_module::DataId;

use super::super::super::Lowering;
use super::super::super::body::{Body, Held};
use super::super::super::fx::{Fx, PTR, unaligned};
use super::super::super::layout::Shape;
use super::super::super::runtime::memory::cell_of;
use super::super::super::runtime::{Routine, report};
use super::super::{Supply, sims};
use super::iommu;
use crate::check::db::ItemId;
use crate::check::sig::{SigKind, int_literal};
use crate::check::ty::{Arg, Args, Const, Ty};
use crate::nir::Operand;
use crate::syntax::ast::{Access, ItemKind};

/// A simulated register block: whether a window is mapped over it, the cell its queue nodes are
/// drawn through, then a descriptor per register and the element slots the descriptors point
/// into.
mod block {
    pub const MAPPED: u64 = 0;
    pub const CELL: u64 = 8;
    pub const DESC: u64 = 16;
    /// A descriptor: the byte offset of the register's first slot, its element count and whether
    /// it is write-one-clear.
    pub const DESC_SIZE: u64 = 16;
    pub const SLOT_BASE: u64 = 0;
    pub const COUNT: u64 = 4;
    pub const WOC: u64 = 8;
}

/// An element's slot: its held value, whether it has a last-written value, that value, and the
/// head and tail of its queue.
mod slot {
    pub const HELD: u64 = 0;
    pub const HAS_LAST: u64 = 8;
    pub const LAST: u64 = 16;
    pub const QHEAD: u64 = 24;
    pub const QTAIL: u64 = 32;
    pub const SIZE: u64 = 40;
    /// A queue node: its value and the next node.
    pub const NODE_VALUE: u64 = 0;
    pub const NODE_NEXT: u64 = 8;
    pub const NODE_SIZE: u64 = 16;
}

/// A DMA buffer, whose value is its box's content pointer: its bus address, contents and box are
/// that pointer; on the host no device reaches it.
pub fn dma(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    match supply.member() {
        "of" => {
            let contents = fx.load(PTR, supply.param(0).address(), 0);
            let sret = supply.sret.expect("a DMA buffer's slot");
            let none = fx.ptr(0);
            fx.store(contents, sret, 0);
            fx.store(none, sret, 8);
            if lo.machine() {
                let space = supply.param(1).address();
                let key = fx.load(PTR, space, 0);
                let domain = fx.b.ins().icmp_imm_s(IntCC::NotEqual, key, 0);
                fx.when(domain, |fx| {
                    let size = lo.layout(&supply.type_arg(0)).size;
                    let (pages, mapping) = map_pages(lo, fx, contents, size, space);
                    fx.store(pages, sret, 0);
                    fx.store(mapping, sret, 8);
                });
            }
            supply.ret(lo, fx, Held::Address(sret));
        }
        "into_box" => {
            let place = supply.param(0).address();
            let contents = fx.load(PTR, place, 0);
            if lo.machine() {
                let mapping = fx.load(PTR, place, 8);
                let mapped = fx.b.ins().icmp_imm_s(IntCC::NotEqual, mapping, 0);
                fx.when(mapped, |fx| {
                    iommu::call_hw(lo, fx, "unmapped", &[Held::Scalar(mapping)]);
                });
            }
            let sret = supply.sret.expect("a box's slot");
            fx.store(contents, sret, 0);
            supply.ret(lo, fx, Held::Address(sret));
        }
        "contents" => {
            let pointer = fx.load(PTR, supply.param(0).address(), 0);
            supply.ret(lo, fx, Held::Address(pointer));
        }
        "write" => {
            let pointer = fx.load(PTR, supply.param(0).address(), 0);
            lo.store_held(fx, pointer, supply.param(1), &supply.type_arg(0));
            supply.done(lo, fx);
        }
        "bus_addr" => {
            let place = supply.param(0).address();
            let pointer = fx.load(PTR, place, 0);
            let sret = supply.sret.expect("a bus address's slot");
            fx.store(pointer, sret, 0);
            supply.ret(lo, fx, Held::Address(sret));
        }
        member => panic!("no lowering of a DMA buffer's {member}"),
    }
}

/// Moves the `size` bytes of a box's contents into whole pages of their own drawn through the
/// box's allocator, then maps those pages in an I/O space's domain; gives the pages and the
/// mapping's record.
fn map_pages(
    lo: &mut Lowering,
    fx: &mut Fx,
    contents: Value,
    size: u64,
    space: Value,
) -> (Value, Value) {
    let bytes = size.max(1).next_multiple_of(PAGE);
    let cell = cell_of(lo, fx, contents);
    let length = fx.ptr(bytes);
    let align = fx.ptr(PAGE);
    let pages = lo.call_routine(fx, Routine::Alloc, &[cell, length, align])[0];
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, pages, 0);
    fx.when(missing, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
    let moved = fx.ptr(size);
    lo.call_routine(fx, Routine::Copy, &[pages, contents, moved]);
    let rest = fx.offset(pages, size);
    let zero = fx.ptr(0);
    let left = fx.ptr(bytes - size);
    lo.call_routine(fx, Routine::Fill, &[rest, zero, left]);
    lo.call_routine(fx, Routine::Free, &[contents]);
    let length = fx.ptr(bytes);
    let mapping = iommu::call_hw(
        lo,
        fx,
        "mapped",
        &[
            Held::Address(space),
            Held::Scalar(pages),
            Held::Scalar(length),
        ],
    );
    (pages, mapping.scalar())
}

/// The bytes of a page an I/O domain maps.
const PAGE: u64 = 4_096;

/// The bit of a window's word set when the window reaches a simulated block, which a machine
/// test image holds beside windows mapped over devices; no port, physical address or block's
/// address has it.
const SIMULATED: i64 = i64::MIN;

/// A register window, which reaches the registers of a simulated block or, on a machine target,
/// those of a device: on the host every window is simulated, a machine image's other than a test
/// image's never are, and a machine test image's word says which one is.
pub fn window(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    if supply.member() == "extent" {
        let Ty::Registers(block) = supply.type_arg(0) else {
            panic!("a registers block");
        };
        let extent = fx.ptr(registers_extent(lo, block));
        supply.ret(lo, fx, Held::Scalar(extent));
        return;
    }
    if supply.member() == "map" {
        assert!(
            lo.machine(),
            "hw::Mmio.map and hw::Ports.map need a machine target"
        );
        let base = supply.param(0).scalar();
        let base = match supply.path.starts_with("hw::Ports") {
            true => fx.b.ins().uextend(PTR, base),
            false => base,
        };
        let sret = supply.sret.expect("a window's slot");
        fx.store(base, sret, 0);
        supply.ret(lo, fx, Held::Address(sret));
        return;
    }
    let word = fx.load(PTR, supply.param(0).address(), 0);
    match (lo.machine(), lo.seeded()) {
        (false, _) => simulated_window(lo, fx, supply, word),
        (true, false) => device_window(lo, fx, supply, word),
        (true, true) => {
            let simulated = fx.b.ins().icmp_imm_s(IntCC::SignedLessThan, word, 0);
            fx.when(simulated, |fx| {
                let block = fx.b.ins().band_imm_s(word, !SIMULATED);
                simulated_window(lo, fx, supply, block);
            });
            device_window(lo, fx, supply, word);
        }
    }
}

/// A window's operation on the simulated block at `block`: `unmap` ends the window, the block
/// staying mapped, and a volatile call acts on the register's slot.
fn simulated_window(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, block: Value) {
    let member = supply.member();
    if member == "unmap" {
        supply.done(lo, fx);
        return;
    }
    let value_ty = supply.type_arg(1);
    let register = supply.param(1).scalar();
    let indexed = member.ends_with("_at");
    let element = match indexed {
        true => supply.param(2).scalar(),
        false => fx.ptr(0),
    };
    let addr = slot_address(lo, fx, block, register, element, indexed);
    if member.starts_with("read") {
        let value = read_slot(lo, fx, &value_ty, addr);
        supply.ret(lo, fx, value);
    } else {
        let value = match indexed {
            true => supply.param(3),
            false => supply.param(2),
        };
        write_slot(lo, fx, block, register, &value_ty, addr, value);
        supply.done(lo, fx);
    }
}

/// The address of a register's or element's slot in a block, trapping when an element index is
/// not below the register's count.
fn slot_address(
    lo: &mut Lowering,
    fx: &mut Fx,
    block: Value,
    register: Value,
    element: Value,
    indexed: bool,
) -> Value {
    let desc = descriptor(fx, block, register);
    if indexed {
        let count = fx.load(types::I32, desc, block::COUNT);
        let count = fx.b.ins().uextend(PTR, count);
        let out =
            fx.b.ins()
                .icmp(IntCC::UnsignedGreaterThanOrEqual, element, count);
        fx.when(out, |fx| report::raise_kind(lo, fx, "trap.bounds"));
    }
    let base = fx.load(types::I32, desc, block::SLOT_BASE);
    let base = fx.b.ins().uextend(PTR, base);
    let base = fx.b.ins().iadd(block, base);
    let offset = fx.b.ins().imul_imm_s(element, slot::SIZE as i64);
    fx.b.ins().iadd(base, offset)
}

/// The descriptor of a register in a block.
fn descriptor(fx: &mut Fx, block: Value, register: Value) -> Value {
    let offset = fx.b.ins().imul_imm_s(register, block::DESC_SIZE as i64);
    let base = fx.b.ins().iadd_imm_s(block, block::DESC as i64);
    fx.b.ins().iadd(base, offset)
}

/// Reads a slot's oldest queued value, or its held value when the queue is empty.
fn read_slot(lo: &mut Lowering, fx: &mut Fx, value_ty: &Ty, addr: Value) -> Held {
    let head = fx.load(PTR, addr, slot::QHEAD);
    let queued = fx.b.ins().icmp_imm_s(IntCC::NotEqual, head, 0);
    let value_at = fx.choose(
        queued,
        &[PTR],
        |fx| {
            let next = fx.load(PTR, head, slot::NODE_NEXT);
            fx.store(next, addr, slot::QHEAD);
            let last = fx.b.ins().icmp_imm_s(IntCC::Equal, next, 0);
            fx.when(last, |fx| {
                let zero = fx.ptr(0);
                fx.store(zero, addr, slot::QTAIL);
            });
            let node = fx.b.ins().iadd_imm_s(head, slot::NODE_VALUE as i64);
            vec![node]
        },
        |fx| vec![fx.b.ins().iadd_imm_s(addr, slot::HELD as i64)],
    )[0];
    lo.held_at(fx, value_ty, value_at)
}

/// Writes a slot's held and last-written values, clearing bits for a write-one-clear register.
fn write_slot(
    lo: &mut Lowering,
    fx: &mut Fx,
    block: Value,
    register: Value,
    value_ty: &Ty,
    addr: Value,
    value: Held,
) {
    let desc = descriptor(fx, block, register);
    let woc = fx.load(types::I32, desc, block::WOC);
    let clears = fx.b.ins().icmp_imm_s(IntCC::NotEqual, woc, 0);
    let value_slot = fx.slot(8, 8);
    let zero = fx.ptr(0);
    fx.store(zero, value_slot, 0);
    lo.store_held(fx, value_slot, value, value_ty);
    let bits = fx.load(PTR, value_slot, 0);
    let held_at = fx.b.ins().iadd_imm_s(addr, slot::HELD as i64);
    let held = fx.load(PTR, held_at, 0);
    let cleared = fx.b.ins().band_not(held, bits);
    let next = fx.b.ins().select(clears, cleared, bits);
    fx.store(next, held_at, 0);
    fx.store(bits, addr, slot::LAST);
    let one = fx.int(types::I8, 1);
    fx.store(one, addr, slot::HAS_LAST);
}

/// The `hw::sim::Registers` methods: building a block from its registers block, the one window
/// over it, and the host-only ways to set, queue and read the last-written value of a register.
pub fn registers(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    match supply.member() {
        "of" => {
            let block_ty = supply.type_arg(0);
            let descriptors = register_descriptors(lo, &block_ty);
            let total = descriptors
                .last()
                .map_or(block::DESC, |last| last.slot_base + last.count * slot::SIZE);
            let cell = fx.load(PTR, supply.param(0).address(), 0);
            let size = fx.ptr(total);
            let block = sims::draw(lo, fx, cell, size);
            fx.store(cell, block, block::CELL);
            for (index, desc) in descriptors.iter().enumerate() {
                let at = fx.b.ins().iadd_imm_s(
                    block,
                    (block::DESC + index as u64 * block::DESC_SIZE) as i64,
                );
                let slot_base = fx.int(types::I32, desc.slot_base as i64);
                fx.store(slot_base, at, block::SLOT_BASE);
                let count = fx.int(types::I32, desc.count as i64);
                fx.store(count, at, block::COUNT);
                let woc = fx.int(types::I32, i64::from(desc.woc));
                fx.store(woc, at, block::WOC);
            }
            let sret = supply.sret.expect("a register block's slot");
            fx.store(block, sret, 0);
            supply.ret(lo, fx, Held::Address(sret));
        }
        "mmio" | "ports" => {
            let block = fx.load(PTR, supply.param(0).address(), 0);
            let mapped = fx.load(types::I8, block, block::MAPPED);
            let twice = fx.b.ins().icmp_imm_s(IntCC::NotEqual, mapped, 0);
            fx.when(twice, |fx| {
                report::raise_kind(lo, fx, "trap.duplicate-access")
            });
            let one = fx.int(types::I8, 1);
            fx.store(one, block, block::MAPPED);
            let word = match lo.machine() {
                true => fx.b.ins().bor_imm_s(block, SIMULATED),
                false => block,
            };
            let sret = supply.sret.expect("a window's slot");
            fx.store(word, sret, 0);
            supply.ret(lo, fx, Held::Address(sret));
        }
        member => sim_access(lo, fx, supply, member),
    }
}

/// A `set`, `queue` or `last_written` on a register's slot.
fn sim_access(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, member: &str) {
    let value_ty = supply.type_arg(1);
    let block = fx.load(PTR, supply.param(0).address(), 0);
    let register = supply.param(1).scalar();
    let indexed = member.ends_with("_at");
    let element = match indexed {
        true => supply.param(2).scalar(),
        false => fx.ptr(0),
    };
    let addr = slot_address(lo, fx, block, register, element, indexed);
    match member.trim_end_matches("_at") {
        "set" => {
            let value = if indexed {
                supply.param(3)
            } else {
                supply.param(2)
            };
            let held_at = fx.b.ins().iadd_imm_s(addr, slot::HELD as i64);
            let value_slot = fx.slot(8, 8);
            let zero = fx.ptr(0);
            fx.store(zero, value_slot, 0);
            lo.store_held(fx, value_slot, value, &value_ty);
            let bits = fx.load(PTR, value_slot, 0);
            fx.store(bits, held_at, 0);
            supply.done(lo, fx);
        }
        "queue" => {
            let value = if indexed {
                supply.param(3)
            } else {
                supply.param(2)
            };
            let cell = fx.load(PTR, block, block::CELL);
            let node_size = fx.ptr(slot::NODE_SIZE);
            let node = sims::draw(lo, fx, cell, node_size);
            let value_at = fx.b.ins().iadd_imm_s(node, slot::NODE_VALUE as i64);
            lo.store_held(fx, value_at, value, &value_ty);
            let tail = fx.load(PTR, addr, slot::QTAIL);
            let has_tail = fx.b.ins().icmp_imm_s(IntCC::NotEqual, tail, 0);
            fx.choose(
                has_tail,
                &[],
                |fx| {
                    fx.store(node, tail, slot::NODE_NEXT);
                    Vec::new()
                },
                |fx| {
                    fx.store(node, addr, slot::QHEAD);
                    Vec::new()
                },
            );
            fx.store(node, addr, slot::QTAIL);
            supply.done(lo, fx);
        }
        "last_written" => {
            let option = supply.abi.result.clone();
            let layout = lo.layout(&option);
            let sret = supply
                .sret
                .unwrap_or_else(|| fx.slot(layout.size, layout.align));
            let has = fx.load(types::I8, addr, slot::HAS_LAST);
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, has, 0);
            let last_at = fx.b.ins().iadd_imm_s(addr, slot::LAST as i64);
            let held = lo.held_at(fx, &value_ty, last_at);
            lo.write_option(fx, &option, sret, present, held);
            let result = lo.held_at(fx, &option, sret);
            supply.ret(lo, fx, result);
        }
        member => panic!("no lowering of hw::sim::Registers.{member}"),
    }
}

/// A register's descriptor computed from the registers block: where its slots start, how many
/// elements it has and whether it is write-one-clear.
struct Descriptor {
    slot_base: u64,
    count: u64,
    woc: bool,
}

/// The descriptors of a registers block's registers, in order.
fn register_descriptors(lo: &mut Lowering, block_ty: &Ty) -> Vec<Descriptor> {
    let Ty::Registers(item) = block_ty else {
        panic!("a registers block, found {block_ty:?}");
    };
    let crate::check::sig::SigKind::Registers { registers } = &lo.ck.sig(*item).kind else {
        panic!("a registers block's signature");
    };
    let registers = registers.clone();
    let mut descriptors = Vec::new();
    let mut base = block::DESC + registers.len() as u64 * block::DESC_SIZE;
    for register in &registers {
        let count = match &register.ty {
            Ty::Item(array, args) if lo.ck.db.canonical(*array) == "core::Array" => {
                match args.get(1) {
                    Some(Arg::Const(Const::Int(count))) => *count as u64,
                    _ => 1,
                }
            }
            _ => 1,
        };
        descriptors.push(Descriptor {
            slot_base: base,
            count,
            woc: register.access == Access::WriteOneClear,
        });
        base += count * slot::SIZE;
    }
    descriptors
}

/// A DMA access call `hw::Dma.access`: its type arguments name the field's bytes layout, its
/// index in it and whether the access writes and is indexed; its operands are the buffer, then an
/// index and a value as the access needs them. On the host the acquire and release orderings are
/// implicit; on a machine target each part is one access through a compiler stub, across which
/// no memory access moves.
pub fn dma_access(body: &mut Body, args: &Args, operands: &[Operand]) -> Held {
    let layout = args[0].ty().expect("a layout type").clone();
    let field_index = match &args[1] {
        Arg::Const(Const::Int(index)) => *index as usize,
        other => panic!("a field index, found {other:?}"),
    };
    let write = matches!(&args[2], Arg::Const(Const::Bool(true)));
    let indexed = matches!(&args[3], Arg::Const(Const::Bool(true)));
    let (receiver, _) = body.operand(&operands[0]);
    let content = body.fx.load(PTR, receiver.address(), 0);
    let layout_rc = body.lo.layout(&layout);
    let field = match &layout_rc.shape {
        Shape::Bytes { fields, .. } => fields[field_index].clone(),
        other => panic!("a bytes layout, found {other:?}"),
    };
    let base = body.fx.b.ins().iadd_imm_s(content, field.offset as i64);
    let (element_ty, addr) = if indexed {
        let element = field
            .ty
            .type_arg(0)
            .cloned()
            .expect("an array element type");
        let count = match &field.ty {
            Ty::Item(_, args) => match args.get(1) {
                Some(Arg::Const(Const::Int(count))) => *count as u64,
                _ => 0,
            },
            _ => 0,
        };
        let (index, _) = body.operand(&operands[1]);
        let index = index.scalar();
        let bound = body.fx.ptr(count);
        let out = body
            .fx
            .b
            .ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, index, bound);
        body.fx.when(out, |fx| {
            report::raise_kind(body.lo, fx, "trap.bounds");
        });
        let size = body.lo.layout(&element).size;
        let offset = body.fx.b.ins().imul_imm_s(index, size as i64);
        (element, body.fx.b.ins().iadd(base, offset))
    } else {
        (field.ty.clone(), base)
    };
    let machine = body.lo.machine();
    match (write, machine) {
        (true, false) => {
            let value_operand = if indexed { &operands[2] } else { &operands[1] };
            let (value, _) = body.operand(value_operand);
            body.lo.store_held(body.fx, addr, value, &element_ty);
            Held::Empty
        }
        (false, false) => body.lo.held_at(body.fx, &element_ty, addr),
        (true, true) => {
            let value_operand = if indexed { &operands[2] } else { &operands[1] };
            let (value, _) = body.operand(value_operand);
            let layout = body.lo.layout(&element_ty);
            let staged = body.fx.slot(layout.size.max(1), layout.align.max(1));
            body.lo.store_held(body.fx, staged, value, &element_ty);
            for (offset, width) in dma_parts(body.lo, &element_ty) {
                let part = body.fx.b.ins().load(
                    Type::int(8 * width as u16).expect("an integer width"),
                    super::super::super::fx::unaligned(),
                    staged,
                    offset as i32,
                );
                let part = match width {
                    8 => part,
                    _ => body.fx.b.ins().uextend(PTR, part),
                };
                let at = body.fx.offset(addr, offset);
                body.lo.mmio_write(body.fx, width, at, part);
            }
            Held::Empty
        }
        (false, true) => {
            let layout = body.lo.layout(&element_ty);
            let staged = body.fx.slot(layout.size.max(1), layout.align.max(1));
            for (offset, width) in dma_parts(body.lo, &element_ty) {
                let at = body.fx.offset(addr, offset);
                let part = body.lo.mmio_read(body.fx, width, at);
                let part = match width {
                    8 => part,
                    _ => body
                        .fx
                        .b
                        .ins()
                        .ireduce(Type::int(8 * width as u16).expect("an integer width"), part),
                };
                body.fx.b.ins().store(
                    super::super::super::fx::unaligned(),
                    part,
                    staged,
                    offset as i32,
                );
            }
            body.lo.held_at(body.fx, &element_ty, staged)
        }
    }
}

/// The integer scalars and integer layouts of a DMA access's field or element, in ascending
/// address order: each one's offset and width, one access each.
fn dma_parts(lo: &mut Lowering, ty: &Ty) -> Vec<(u64, u64)> {
    let layout = lo.layout(ty);
    match &layout.shape {
        Shape::Scalar { ty, .. } => vec![(0, u64::from(ty.bytes()))],
        Shape::Bytes { fields, .. } => {
            let mut parts = Vec::new();
            for field in fields.clone() {
                for (offset, width) in dma_parts(lo, &field.ty) {
                    parts.push((field.offset + offset, width));
                }
            }
            parts
        }
        Shape::Array { element, count } => {
            let (element, count) = (element.clone(), *count);
            let size = lo.layout(&element).size;
            let inner = dma_parts(lo, &element);
            (0..count)
                .flat_map(|index| {
                    inner
                        .iter()
                        .map(move |(offset, width)| (index * size + offset, *width))
                })
                .collect()
        }
        other => panic!("a DMA access of a layout, found {other:?}"),
    }
}

/// The bytes of a register's entry in the table of its registers block a device window reads:
/// its byte offset, then its stride, each 4 bytes.
const REGISTER_ENTRY: u64 = 8;

/// The bytes of a registers block from its offset 0 to past its last register or element.
fn registers_extent(lo: &mut Lowering, block: ItemId) -> u64 {
    let ItemKind::Registers(declared) = &lo.ck.db.ast(block).kind else {
        panic!("a registers item");
    };
    let data = lo.ck.db.item(block);
    let number = |token| int_literal(data.text(token)).unwrap_or(0) as u64;
    let SigKind::Registers { registers } = &lo.ck.sig(block).kind else {
        panic!("a registers block");
    };
    let mut extent = 0;
    for (register, sig) in declared.registers.iter().zip(registers) {
        let (size, last) = match (&sig.ty, register.stride) {
            (Ty::Item(_, args), Some(stride)) => {
                let element = args[0].ty().cloned().expect("an array element type");
                let count = match &args[1] {
                    Arg::Const(Const::Int(count)) => *count as u64,
                    _ => 1,
                };
                (lo.layout(&element).size, (count - 1) * number(stride))
            }
            _ => (lo.layout(&sig.ty).size, 0),
        };
        extent = extent.max(number(register.at) + last + size);
    }
    extent
}

/// The table of a registers block, an entry per register in order.
fn register_table(lo: &mut Lowering, block: ItemId) -> DataId {
    let ItemKind::Registers(declared) = &lo.ck.db.ast(block).kind else {
        panic!("a registers item");
    };
    let data = lo.ck.db.item(block);
    let number = |token| int_literal(data.text(token)).unwrap_or(0) as u32;
    let mut bytes = Vec::new();
    for register in &declared.registers {
        bytes.extend_from_slice(&number(register.at).to_le_bytes());
        let stride = register.stride.map_or(0, number);
        bytes.extend_from_slice(&stride.to_le_bytes());
    }
    lo.bytes_data(&bytes)
}

/// A window's operation on the device whose port or physical address is `base`: `unmap` ends
/// the window, and a volatile call reaches the register, an array register's element at its
/// stride, through the port compiler stubs or a memory-mapped access.
fn device_window(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, base: Value) {
    let member = supply.member();
    if member == "unmap" {
        supply.done(lo, fx);
        return;
    }
    let Ty::Registers(block) = supply.type_arg(0) else {
        panic!("a registers block window");
    };
    let element_ty = supply.type_arg(1);
    let width = lo.layout(&element_ty).size;
    let table = register_table(lo, block);
    let table = lo.data_address(fx, table);
    let register = supply.param(1).scalar();
    let entry = fx.b.ins().imul_imm_s(register, REGISTER_ENTRY as i64);
    let entry = fx.b.ins().iadd(table, entry);
    let offset = fx.b.ins().load(types::I32, unaligned(), entry, 0);
    let offset = fx.b.ins().uextend(PTR, offset);
    let mut addr = fx.b.ins().iadd(base, offset);
    let indexed = member.ends_with("_at");
    if indexed {
        let count = match supply.args.get(2) {
            Some(Arg::Const(Const::Int(count))) => *count as i64,
            other => panic!("an array register's length, found {other:?}"),
        };
        let index = supply.param(2).scalar();
        let out =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, index, count);
        fx.when(out, |fx| report::raise_kind(lo, fx, "trap.bounds"));
        let stride = fx.b.ins().load(types::I32, unaligned(), entry, 4);
        let stride = fx.b.ins().uextend(PTR, stride);
        let step = fx.b.ins().imul(index, stride);
        addr = fx.b.ins().iadd(addr, step);
    }
    let ports = supply.path.starts_with("hw::Ports");
    if member.starts_with("read") {
        let value = match ports {
            true => lo.port_in(fx, width, addr),
            false => lo.mmio_read(fx, width, addr),
        };
        let slot = fx.slot(8, 8);
        fx.store(value, slot, 0);
        let held = lo.held_at(fx, &element_ty, slot);
        supply.ret(lo, fx, held);
    } else {
        let value = match indexed {
            true => supply.param(3),
            false => supply.param(2),
        };
        let slot = fx.slot(8, 8);
        let zero = fx.ptr(0);
        fx.store(zero, slot, 0);
        lo.store_held(fx, slot, value, &element_ty);
        let bits = fx.load(PTR, slot, 0);
        match ports {
            true => lo.port_out(fx, width, addr, bits),
            false => lo.mmio_write(fx, width, addr, bits),
        }
        supply.done(lo, fx);
    }
}
