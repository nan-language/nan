//! The firmware tables a machine image reads: the UEFI configuration table, and the ACPI tables
//! the root system description pointer reaches.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::fx::{Fx, PTR, unaligned};
use super::super::runtime::state;
use super::start::info;
use crate::workspace::{Arch, Kind};

/// The offsets in a UEFI system table of its configuration table's entry count and address, the
/// size of an entry, a GUID then the table's address, and the GUIDs of the ACPI tables and of
/// the flattened device tree, each as its two little-endian words.
const SYSTEM_TABLE_ENTRIES: u64 = 104;
const SYSTEM_TABLE_CONFIGURATION: u64 = 112;
const CONFIGURATION_ENTRY: u64 = 24;
const ACPI_10_TABLE: (u64, u64) = (0x11D3_2D88_EB9D_2D30, 0x4DC1_3F27_9000_169A);
const ACPI_20_TABLE: (u64, u64) = (0x11D3_E4F1_8868_E871, 0x8188_3CC7_8000_22BC);
pub(super) const DEVICE_TREE_TABLE: (u64, u64) = (0x41A5_F19C_B1B6_21D5, 0xE0AA_692C_15D9_0B83);

/// The size of an ACPI table's header, the MADT's signature, and the offset of its first entry.
const SDT_HEADER: u64 = 36;
const MADT_SIGNATURE: i64 = 0x4349_5041;
const MADT_ENTRIES: u64 = 44;

/// The BIOS area the RSDP may lie in, on a 16-byte boundary, and its signature.
const BIOS_AREA: (u64, u64) = (0xE_0000, 0x10_0000);
const RSDP_SIGNATURE: u64 = 0x2052_5450_2044_5352;

impl Lowering<'_, '_> {
    /// The address of the ACPI root system description pointer, zero when the platform gives
    /// none: on a bare target the one the start information names, or else on `x86_64-bare` the
    /// one found in the BIOS area; on a UEFI target the one the firmware's configuration table
    /// names, preferring the ACPI 2.0 entry.
    pub fn rsdp(&mut self, fx: &mut Fx) -> Value {
        let state_address = self.state(fx);
        match self.ck.db.target.kind() {
            Some(Kind::Uefi) => {
                let table = fx.load(PTR, state_address, state::PLATFORM_TABLE);
                return self.configured(fx, table, &[ACPI_10_TABLE, ACPI_20_TABLE]);
            }
            Some(Kind::Bare) => {}
            _ => return fx.ptr(0),
        }
        let info = self.start_info(fx);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, info, 0);
        let given = fx.choose(
            present,
            &[PTR],
            |fx| vec![fx.load(PTR, info, info::RSDP)],
            |fx| vec![fx.ptr(0)],
        )[0];
        if self.ck.db.target.arch() == Some(Arch::Aarch64) {
            return given;
        }
        let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, given, 0);
        fx.choose(missing, &[PTR], |fx| vec![scan_rsdp(fx)], |_| vec![given])[0]
    }

    /// The address a UEFI system table's configuration table gives for the last of `guids` it
    /// lists, zero when it lists none.
    pub(super) fn configured(&mut self, fx: &mut Fx, table: Value, guids: &[(u64, u64)]) -> Value {
        let count = fx.load(PTR, table, SYSTEM_TABLE_ENTRIES);
        let entries = fx.load(PTR, table, SYSTEM_TABLE_CONFIGURATION);
        let zero = fx.ptr(0);
        let found = fx.var(PTR, zero);
        for guid in guids {
            fx.each(zero, count, |fx, index| {
                let entry = fx.b.ins().imul_imm_s(index, CONFIGURATION_ENTRY as i64);
                let entry = fx.b.ins().iadd(entries, entry);
                let low = fx.load(PTR, entry, 0);
                let high = fx.load(PTR, entry, 8);
                let low = fx.b.ins().icmp_imm_s(IntCC::Equal, low, guid.0 as i64);
                let high = fx.b.ins().icmp_imm_s(IntCC::Equal, high, guid.1 as i64);
                let same = fx.b.ins().band(low, high);
                fx.when(same, |fx| {
                    let pointer = fx.load(PTR, entry, 16);
                    fx.b.def_var(found, pointer);
                });
            });
        }
        fx.b.use_var(found)
    }

    /// The ACPI table of a signature the RSDP's XSDT or RSDT lists, zero when none.
    pub(super) fn acpi_table(&mut self, fx: &mut Fx, rsdp: Value, signature: i64) -> Value {
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, rsdp, 0);
        fx.choose(
            present,
            &[PTR],
            |fx| {
                let revision = fx.b.ins().load(types::I8, unaligned(), rsdp, 15);
                let extended =
                    fx.b.ins()
                        .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, revision, 2);
                let xsdt = fx.choose(
                    extended,
                    &[PTR],
                    |fx| {
                        let xsdt = fx.b.ins().load(PTR, unaligned(), rsdp, 24);
                        vec![xsdt]
                    },
                    |fx| vec![fx.ptr(0)],
                )[0];
                let wide = fx.b.ins().icmp_imm_s(IntCC::NotEqual, xsdt, 0);
                let rsdt = load_u32(fx, rsdp, 16);
                let table = fx.b.ins().select(wide, xsdt, rsdt);
                let eight = fx.ptr(8);
                let four = fx.ptr(4);
                let width = fx.b.ins().select(wide, eight, four);
                let length = load_u32(fx, table, 4);
                let listed = fx.b.ins().iadd_imm_s(length, -(SDT_HEADER as i64));
                let count = fx.b.ins().udiv(listed, width);
                let zero = fx.ptr(0);
                let found = fx.var(PTR, zero);
                fx.each(zero, count, |fx, index| {
                    let offset = fx.b.ins().imul(index, width);
                    let at = fx.b.ins().iadd(table, offset);
                    let at = fx.b.ins().iadd_imm_s(at, SDT_HEADER as i64);
                    let entry = fx.choose(
                        wide,
                        &[PTR],
                        |fx| vec![fx.b.ins().load(PTR, unaligned(), at, 0)],
                        |fx| vec![load_u32(fx, at, 0)],
                    )[0];
                    let listed = load_u32(fx, entry, 0);
                    let same = fx.b.ins().icmp_imm_s(IntCC::Equal, listed, signature);
                    fx.when(same, |fx| fx.b.def_var(found, entry));
                });
                vec![fx.b.use_var(found)]
            },
            |fx| vec![fx.ptr(0)],
        )[0]
    }

    /// Visits each entry of the MADT the RSDP lists, given its address and its type, while the
    /// entry's header lies in the table and its length is nonzero.
    pub(super) fn madt_entries(&mut self, fx: &mut Fx, rsdp: Value, visit: MadtVisit) {
        let madt = self.acpi_table(fx, rsdp, MADT_SIGNATURE);
        let found = fx.b.ins().icmp_imm_s(IntCC::NotEqual, madt, 0);
        fx.when(found, |fx| {
            let length = load_u32(fx, madt, 4);
            let end = fx.b.ins().iadd(madt, length);
            let first = fx.b.ins().iadd_imm_s(madt, MADT_ENTRIES as i64);
            fx.repeat(&[first], |fx, at| {
                let at = at[0];
                let header = fx.b.ins().iadd_imm_s(at, 2);
                let inside = fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, header, end);
                let next = fx.choose_in(
                    self,
                    inside,
                    &[PTR, types::I8],
                    |lo, fx| {
                        let kind = fx.b.ins().load(types::I8, unaligned(), at, 0);
                        let len = fx.b.ins().load(types::I8, unaligned(), at, 1);
                        let len = fx.b.ins().uextend(PTR, len);
                        visit(lo, fx, at, kind);
                        let next = fx.b.ins().iadd(at, len);
                        let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
                        vec![next, more]
                    },
                    |_, fx| vec![at, fx.int(types::I8, 0)],
                );
                (next[1], vec![next[0]])
            });
        });
    }
}

/// What a walk of the MADT does with each entry, given its address and its type.
pub(super) type MadtVisit<'a> = &'a mut dyn FnMut(&mut Lowering, &mut Fx, Value, Value);

/// The RSDP found in the BIOS area, zero when none.
fn scan_rsdp(fx: &mut Fx) -> Value {
    let start = fx.ptr(BIOS_AREA.0);
    let zero = fx.ptr(0);
    let found = fx.repeat(&[start, zero], |fx, state| {
        let (at, _) = (state[0], state[1]);
        let word = fx.load(PTR, at, 0);
        let signature = fx.ptr(RSDP_SIGNATURE);
        let same = fx.b.ins().icmp(IntCC::Equal, word, signature);
        let zero = fx.ptr(0);
        let found = fx.b.ins().select(same, at, zero);
        let next = fx.b.ins().iadd_imm_s(at, 16);
        let inside =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedLessThan, next, BIOS_AREA.1 as i64);
        let missing = fx.b.ins().bxor_imm_s(same, 1);
        let more = fx.b.ins().band(inside, missing);
        (more, vec![next, found])
    });
    found[1]
}

/// A 32-bit value of a firmware table at an address, zero-extended.
pub(super) fn load_u32(fx: &mut Fx, address: Value, offset: u64) -> Value {
    let value =
        fx.b.ins()
            .load(types::I32, unaligned(), address, offset as i32);
    fx.b.ins().uextend(PTR, value)
}

/// A 64-bit value of a firmware table at an address.
pub(super) fn load_u64(fx: &mut Fx, address: Value, offset: u64) -> Value {
    fx.b.ins().load(PTR, unaligned(), address, offset as i32)
}
