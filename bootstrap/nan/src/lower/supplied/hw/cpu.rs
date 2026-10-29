//! The CPU-facing operations of `hw`: the context of an interrupt fn, the privileged intrinsics
//! of `hw::Ring0`, the interrupt descriptor table `hw::Idt.of_vectors` builds, the interrupted
//! state an exception's `hw::TrapFrame` names, and copies from user memory through
//! `hw::UserAddr`. Only an interrupt fn's context exists on the host, where no interrupt fires.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::super::Lowering;
use super::super::super::body::Held;
use super::super::super::fx::{Fx, PTR};
use super::super::super::machine::Stub;
use super::super::super::machine::{A64_ACCESS, A64_TABLE, A64_VALID, LARGE, PRESENT};
use super::super::{Supply, sys};

/// `hw::IrqContext`: on the host completing one does nothing and its vector reads as zero; on a
/// machine target completing one ends the interrupt the running CPU serves, whose vector is its
/// vector.
pub fn irq_context(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    match supply.member() {
        "complete" => {
            if lo.machine() {
                lo.end_of_interrupt(fx);
            }
            supply.done(lo, fx);
        }
        "vector" => {
            let vector = match lo.machine() {
                true => lo.current_vector(fx),
                false => fx.int(types::I8, 0),
            };
            supply.ret(lo, fx, Held::Scalar(vector));
        }
        "irqs_disabled" => supply.ret(lo, fx, Held::Empty),
        member => panic!("no lowering of hw::IrqContext.{member}"),
    }
}

/// The privileged intrinsics, each one or more compiler stubs on the running CPU, dispatched by
/// the target's architecture, since each architecture's descriptor names its own set.
pub fn ring0(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    match lo.ck.db.target.arch() {
        Some(crate::workspace::Arch::Aarch64) => ring0_aarch64(lo, fx, supply),
        _ => ring0_x64(lo, fx, supply),
    }
}

/// The x86_64 privileged intrinsics. `load_idt` loads the table the box holds and keeps the box
/// in its token; `write_cr3` installs a root and gives the one it replaces, which the CPU's
/// record keeps, none for the entry root.
fn ring0_x64(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    match supply.member() {
        "load_idt" => {
            let table = word(fx, supply.param(1));
            lo.load_idt(fx, table);
            word_result(lo, fx, supply, table);
        }
        "write_cr3" => {
            let root = word(fx, supply.param(1));
            let replaced = lo.swap_root(fx, root);
            let option = supply.abi.result.clone();
            let entry = fx.b.ins().icmp_imm_s(IntCC::Equal, replaced, 0);
            fx.when(entry, |fx| {
                let none = lo.none_value(fx, &option);
                supply.ret(lo, fx, none);
            });
            let held = fx.slot(8, 8);
            fx.store(replaced, held, 0);
            let some = lo.variant_value(fx, &option, "Some", &[Held::Address(held)], None);
            supply.ret(lo, fx, some);
        }
        "invalidate_page" => {
            let page = word(fx, supply.param(1));
            lo.call_stub(fx, Stub::Invlpg, &[page]);
            supply.done(lo, fx);
        }
        "read_msr" => {
            let msr = supply.param(1).scalar();
            let msr = fx.b.ins().uextend(PTR, msr);
            let value = lo.call_stub(fx, Stub::Rdmsr, &[msr])[0];
            supply.ret(lo, fx, Held::Scalar(value));
        }
        "write_msr" => {
            let msr = supply.param(1).scalar();
            let msr = fx.b.ins().uextend(PTR, msr);
            let value = supply.param(2).scalar();
            lo.call_stub(fx, Stub::Wrmsr, &[msr, value]);
            supply.done(lo, fx);
        }
        member => panic!("no x86_64 lowering of hw::Ring0.{member}"),
    }
}

/// The AArch64 system register each `hw::SysReg` variant names, by the variant's name, as the
/// 15-bit field the `mrs` and `msr` instructions hold ([`machine::Stub`]).
const SYS_REGS: [(&str, u16); 24] = [
    ("SctlrEl1", 0x4080),
    ("TcrEl1", 0x4102),
    ("MairEl1", 0x4510),
    ("Ttbr1El1", 0x4101),
    ("ContextidrEl1", 0x4681),
    ("CpacrEl1", 0x4082),
    ("CntkctlEl1", 0x4708),
    ("TpidrEl0", 0x5E82),
    ("TpidrroEl0", 0x5E83),
    ("CntpCtlEl0", 0x5F11),
    ("CntpCvalEl0", 0x5F12),
    ("CntpTvalEl0", 0x5F10),
    ("CntvCtlEl0", 0x5F19),
    ("CntvCvalEl0", 0x5F1A),
    ("CntvTvalEl0", 0x5F18),
    ("MidrEl1", 0x4000),
    ("MpidrEl1", 0x4005),
    ("CntfrqEl0", 0x5F00),
    ("CntpctEl0", 0x5F01),
    ("CntvctEl0", 0x5F02),
    ("CtrEl0", 0x5801),
    ("IdAa64pfr0El1", 0x4020),
    ("IdAa64isar0El1", 0x4030),
    ("IdAa64mmfr0El1", 0x4038),
];

/// How many of `SYS_REGS`, from the first, `hw::SysReg.is_writable` holds for, which
/// `write_sysreg` alone reaches.
const WRITABLE_SYS_REGS: usize = 15;

/// The AArch64 privileged intrinsics. `write_ttbr0` installs a low translation-table root and
/// gives the one it replaces, as `write_cr3` does; `read_sysreg` and `write_sysreg` read and
/// write the system register their `hw::SysReg` names, each built as the instruction holding its
/// encoding; `invalidate_page` removes a page's cached translations.
fn ring0_aarch64(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    match supply.member() {
        "write_ttbr0" => {
            let root = word(fx, supply.param(1));
            let replaced = lo.swap_root(fx, root);
            let option = supply.abi.result.clone();
            let entry = fx.b.ins().icmp_imm_s(IntCC::Equal, replaced, 0);
            fx.when(entry, |fx| {
                let none = lo.none_value(fx, &option);
                supply.ret(lo, fx, none);
            });
            let held = fx.slot(8, 8);
            fx.store(replaced, held, 0);
            let some = lo.variant_value(fx, &option, "Some", &[Held::Address(held)], None);
            supply.ret(lo, fx, some);
        }
        "invalidate_page" => {
            let page = word(fx, supply.param(1));
            lo.call_stub(fx, Stub::TlbiPage, &[page]);
            supply.done(lo, fx);
        }
        "read_sysreg" => {
            let index = sysreg_index(lo, fx, supply);
            for (position, (_, field)) in SYS_REGS.iter().enumerate() {
                let is = fx.b.ins().icmp_imm_s(IntCC::Equal, index, position as i64);
                let field = *field;
                fx.when(is, |fx| {
                    let value = lo.call_stub(fx, Stub::ReadSysReg(field), &[])[0];
                    supply.ret(lo, fx, Held::Scalar(value));
                });
            }
            fx.b.ins()
                .trap(cranelift_codegen::ir::TrapCode::user(1).expect("a trap code"));
        }
        "write_sysreg" => {
            let index = sysreg_index(lo, fx, supply);
            let value = supply.param(2).scalar();
            for (position, (_, field)) in SYS_REGS.iter().take(WRITABLE_SYS_REGS).enumerate() {
                let is = fx.b.ins().icmp_imm_s(IntCC::Equal, index, position as i64);
                let field = *field;
                fx.when(is, |fx| {
                    lo.call_stub(fx, Stub::WriteSysReg(field), &[value]);
                    supply.done(lo, fx);
                });
            }
            fx.b.ins()
                .trap(cranelift_codegen::ir::TrapCode::user(1).expect("a trap code"));
        }
        member => panic!("no AArch64 lowering of hw::Ring0.{member}"),
    }
}

/// The index of the `hw::SysReg` an intrinsic's register parameter holds, in variant order.
fn sysreg_index(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> Value {
    let register_ty = supply.abi.params[1].0.ty().clone();
    let slot = fx.slot(8, 8);
    lo.store_held(fx, slot, supply.param(1), &register_ty);
    lo.variant_index(fx, &register_ty, slot)
}

/// `hw::Idt.of_vectors()`: the table whose gate n enters the vector seam's stub for n.
pub fn idt(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let sret = supply.sret.expect("a table's slot");
    lo.fill_idt(fx, sret);
    supply.ret(lo, fx, Held::Address(sret));
}

/// `hw::TrapFrame[V]`, which names the state the seam saved for an exception: `info` holds the
/// error code the CPU pushed on x86_64 or the syndrome on AArch64 and, for a page fault, the
/// faulting address; `ip` and `set_ip` read and replace the interrupted instruction's address.
pub fn trap_frame(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let trap_frame = supply.param(0).address();
    let owner = supply.owner.clone().expect("a trap frame type");
    let saved_at = lo.layouts.field(lo.ck, &owner, None, "saved").offset;
    let fault_at = lo.layouts.field(lo.ck, &owner, None, "fault").offset;
    let saved = fx.load(PTR, trap_frame, saved_at);
    match supply.member() {
        "info" => {
            let info_ty = supply.type_arg(0);
            let layout = lo.layout(&info_ty);
            let info = fx.slot(layout.size.max(1), layout.align.max(1));
            let canonical = match &info_ty {
                crate::check::ty::Ty::Item(item, _) => lo.ck.db.canonical(*item),
                _ => String::new(),
            };
            if canonical != "hw::NoCode" {
                let code = fx.load(PTR, saved, lo.saved_frame().code);
                let at = lo.layouts.field(lo.ck, &info_ty, None, "code").offset;
                fx.store(code, info, at);
            }
            if canonical == "hw::PageFault" {
                let address = fx.load(PTR, trap_frame, fault_at);
                let at = lo.layouts.field(lo.ck, &info_ty, None, "address").offset;
                fx.store(address, info, at);
            }
            let held = lo.held_at(fx, &info_ty, info);
            supply.ret(lo, fx, held);
        }
        "ip" => {
            let ip = fx.load(PTR, saved, lo.saved_frame().ip);
            supply.ret(lo, fx, Held::Scalar(ip));
        }
        "set_ip" => {
            let ip = supply.param(1).scalar();
            fx.store(ip, saved, lo.saved_frame().ip);
            supply.done(lo, fx);
        }
        "irqs_disabled" => supply.ret(lo, fx, Held::Empty),
        member => panic!("no lowering of hw::TrapFrame.{member}"),
    }
}

/// The upper end of each architecture's user address space, below which `copy_in` reads: the
/// x86_64 canonical lower half and the AArch64 `TTBR0_EL1` range.
const USER_END_X64: i64 = 0x0000_8000_0000_0000;
const USER_END_AARCH64: i64 = 0x0001_0000_0000_0000;

/// The x86_64 bit of a page-table entry granting user mode access, and the mask of an entry's
/// frame address.
const USER: i64 = 0x4;
const FRAME_ADDRESS: i64 = 0x000F_FFFF_FFFF_F000;

/// The AArch64 descriptor bits `copy_in` reads beside the valid, table and access bits:
/// unprivileged access, whose `AP[1]` bit is set for an EL0-readable mapping, and `APTable[0]`,
/// set in a table descriptor to forbid EL0 access to everything below it; and the mask of a
/// descriptor's output address, which leaves out the `TTBR0_EL1` address space identifier.
const A64_EL0: i64 = 0x40;
const A64_TABLE_NO_EL0: i64 = 1 << 61;
const A64_ADDRESS: i64 = 0x0000_FFFF_FFFF_F000;

/// `hw::UserAddr[T]`, one word: `copy_in` fails with `OutsideUser` when the `T` at the address
/// does not lie below the user address space's end, and with `Unmapped` when a page of it is not
/// mapped readable for user mode in the running CPU's page tables; otherwise it copies the bytes
/// once each.
pub fn user_addr(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    match supply.member() {
        "of" => word_result(lo, fx, supply, supply.param(0).scalar()),
        "value" => {
            let value = word(fx, supply.param(0));
            supply.ret(lo, fx, Held::Scalar(value));
        }
        "copy_in" => copy_in(lo, fx, supply),
        member => panic!("no lowering of hw::UserAddr.{member}"),
    }
}

fn copy_in(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let aarch64 = lo.ck.db.target.arch() == Some(crate::workspace::Arch::Aarch64);
    let start = word(fx, supply.param(0));
    let value_ty = supply.type_arg(0);
    let size = lo.layout(&value_ty).size;
    let size_value = fx.ptr(size);
    let (end, wrapped) = fx.b.ins().uadd_overflow(start, size_value);
    let user_end = if aarch64 {
        USER_END_AARCH64
    } else {
        USER_END_X64
    };
    let beyond =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThan, end, user_end);
    let outside = fx.b.ins().bor(beyond, wrapped);
    fx.when(outside, |fx| sys::fail_with(lo, fx, supply, "OutsideUser"));
    let first = fx.b.ins().band_imm_s(start, !0xFFF);
    let pages = fx.b.ins().iadd_imm_s(end, 0xFFF);
    let pages = fx.b.ins().isub(pages, first);
    let pages = fx.b.ins().ushr_imm_s(pages, 12);
    let layout = lo.layout(&value_ty);
    let copied = fx.slot(size.max(1), layout.align.max(1));
    let flags = lo.call_stub(fx, Stub::IrqSave, &[])[0];
    let read_root = if aarch64 {
        Stub::ReadTtbr0
    } else {
        Stub::ReadCr3
    };
    let root = lo.call_stub(fx, read_root, &[])[0];
    let yes = fx.int(types::I8, 1);
    let mapped = fx.var(types::I8, yes);
    let zero = fx.ptr(0);
    fx.each(zero, pages, |fx, index| {
        let offset = fx.b.ins().ishl_imm_s(index, 12);
        let page = fx.b.ins().iadd(first, offset);
        let readable = match aarch64 {
            true => user_readable_aarch64(fx, root, page),
            false => user_readable_x64(fx, root, page),
        };
        let so_far = fx.b.use_var(mapped);
        let so_far = fx.b.ins().band(so_far, readable);
        fx.b.def_var(mapped, so_far);
    });
    let mapped = fx.b.use_var(mapped);
    fx.when(mapped, |fx| lo.copy(fx, copied, start, size, 1));
    lo.call_stub(fx, Stub::IrqRestore, &[flags]);
    let unmapped = fx.b.ins().bxor_imm_s(mapped, 1);
    fx.when(unmapped, |fx| sys::fail_with(lo, fx, supply, "Unmapped"));
    let held = lo.held_at(fx, &supply.abi.result, copied);
    supply.ret(lo, fx, held);
}

/// Whether the x86_64 page tables from `root` map the page at `page` present and user-accessible
/// at every level, a large page ending the walk; the tables lie at their physical addresses.
fn user_readable_x64(fx: &mut Fx, root: Value, page: Value) -> Value {
    let table = fx.b.ins().band_imm_s(root, FRAME_ADDRESS);
    let top = fx.ptr(1);
    let readable = fx.int(types::I8, 1);
    let walked = fx.repeat(&[table, top, readable], |fx, state| {
        let (table, level, readable) = (state[0], state[1], state[2]);
        let shift = fx.b.ins().imul_imm_s(level, -9);
        let shift = fx.b.ins().iadd_imm_s(shift, 48);
        let index = fx.b.ins().ushr(page, shift);
        let index = fx.b.ins().band_imm_s(index, 0x1FF);
        let slot = fx.b.ins().imul_imm_s(index, 8);
        let slot = fx.b.ins().iadd(table, slot);
        let entry = fx.load(PTR, slot, 0);
        let flags = fx.b.ins().band_imm_s(entry, PRESENT as i64 | USER);
        let allowed =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, flags, PRESENT as i64 | USER);
        let readable = fx.b.ins().band(readable, allowed);
        let large = fx.b.ins().band_imm_s(entry, LARGE as i64);
        let large = fx.b.ins().icmp_imm_s(IntCC::NotEqual, large, 0);
        let leaf = fx.b.ins().icmp_imm_s(IntCC::Equal, level, 4);
        let last = fx.b.ins().bor(large, leaf);
        let ended = fx.b.ins().bxor_imm_s(allowed, 1);
        let last = fx.b.ins().bor(last, ended);
        let more = fx.b.ins().bxor_imm_s(last, 1);
        let next = fx.b.ins().band_imm_s(entry, FRAME_ADDRESS);
        let level = fx.b.ins().iadd_imm_s(level, 1);
        (more, vec![next, level, readable])
    });
    walked[2]
}

/// Whether the AArch64 page tables from `root` map the page at `page` EL0-readable: each table
/// descriptor on the way valid without `APTable[0]`, and the descriptor ending the walk a valid,
/// accessed and EL0-readable block at level 1 or 2 or page at level 3. The 4 KiB-granule walk
/// reads levels 0 to 3, each indexed by nine bits of the address from bit 39 down, the tables
/// lying at their physical addresses.
fn user_readable_aarch64(fx: &mut Fx, root: Value, page: Value) -> Value {
    let table = fx.b.ins().band_imm_s(root, A64_ADDRESS);
    let top = fx.ptr(0);
    let readable = fx.int(types::I8, 1);
    let walked = fx.repeat(&[table, top, readable], |fx, state| {
        let (table, level, readable) = (state[0], state[1], state[2]);
        let shift = fx.b.ins().imul_imm_s(level, -9);
        let shift = fx.b.ins().iadd_imm_s(shift, 39);
        let index = fx.b.ins().ushr(page, shift);
        let index = fx.b.ins().band_imm_s(index, 0x1FF);
        let slot = fx.b.ins().imul_imm_s(index, 8);
        let slot = fx.b.ins().iadd(table, slot);
        let entry = fx.load(PTR, slot, 0);
        let valid = fx.b.ins().band_imm_s(entry, A64_VALID as i64);
        let valid = fx.b.ins().icmp_imm_s(IntCC::NotEqual, valid, 0);
        let table_bit = fx.b.ins().band_imm_s(entry, A64_TABLE as i64);
        let table_bit = fx.b.ins().icmp_imm_s(IntCC::NotEqual, table_bit, 0);
        let top = fx.b.ins().icmp_imm_s(IntCC::Equal, level, 0);
        let bottom = fx.b.ins().icmp_imm_s(IntCC::Equal, level, 3);
        // At levels 0 to 2 a table descriptor continues the walk. A block, whose table bit is
        // clear, ends it at level 1 or 2, and a page, whose table bit is set, at level 3; any
        // other descriptor maps nothing.
        let walks = fx.b.ins().band(valid, table_bit);
        let walks = fx.b.ins().band_not(walks, bottom);
        let edge = fx.b.ins().bor(top, bottom);
        let not_block = fx.b.ins().bor(table_bit, edge);
        let block = fx.b.ins().bxor_imm_s(not_block, 1);
        let page_leaf = fx.b.ins().band(table_bit, bottom);
        let leaf = fx.b.ins().bor(block, page_leaf);
        let el0 = fx.b.ins().band_imm_s(entry, A64_EL0 | A64_ACCESS as i64);
        let el0 =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, el0, A64_EL0 | A64_ACCESS as i64);
        let leaf_allowed = fx.b.ins().band(valid, leaf);
        let leaf_allowed = fx.b.ins().band(leaf_allowed, el0);
        let open = fx.b.ins().band_imm_s(entry, A64_TABLE_NO_EL0);
        let open = fx.b.ins().icmp_imm_s(IntCC::Equal, open, 0);
        let more = fx.b.ins().band(walks, open);
        let allowed = fx.b.ins().bor(more, leaf_allowed);
        let readable = fx.b.ins().band(readable, allowed);
        let next = fx.b.ins().band_imm_s(entry, A64_ADDRESS);
        let level = fx.b.ins().iadd_imm_s(level, 1);
        (more, vec![next, level, readable])
    });
    walked[2]
}

/// The word a one-word value holds, whether held by address or as a scalar.
pub fn word(fx: &mut Fx, held: Held) -> Value {
    match held {
        Held::Address(address) => fx.load(PTR, address, 0),
        other => other.scalar(),
    }
}

/// Returns the operation's one-word result holding `value`.
pub fn word_result(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, value: Value) {
    let slot = fx.slot(8, 8);
    fx.store(value, slot, 0);
    let held = lo.held_at(fx, &supply.abi.result, slot);
    supply.ret(lo, fx, held);
}
