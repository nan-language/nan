//! Loading: the load ABI a kernel exports and the bundles built against its interface import,
//! the table records of loaded registries, the gates through which the entries a bundle adds
//! call its functions, and the load window into which the loader maps bundles.
//!
//! A kernel's export table `_NSexports` holds its interface hash, the address of its line
//! tables, the loader's lock, the address of the load window's table of 1 GiB entries, the next
//! address of the window the loader maps, the loader's list of retirements, its list of free
//! window tables, the export count and the address of each export, in the order its interface file lists their
//! names. The table record of a loaded registry holds the address of its current version, then
//! the count of its entry type's function fields and, for each, its offset and the gate calling
//! through it. A version holds an entry array's address and length, the count of reads holding
//! it, and whether the loader made it. A gate's environment is a gate record holding the address
//! of its target record, zero once stale; a target record holds the code of a bundle's function
//! and the count of calls running it.

use std::rc::Rc;

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, condcodes::IntCC};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};

use super::data::Reserved;
use super::fx::{Fx, PTR};
use super::machine::{self, Stub};
use super::runtime::{self, Routine, fetch_add, holdings, report};
use super::stack::{self, Demand, Judged, Report, Site};
use super::{Glue, Instance, Lowering, lines};
use crate::check::db::ItemId;
use crate::check::sig::SigKind;
use crate::check::ty::{FnTy, Scalar, Ty};
use crate::interp::{FnVal, Val};
use crate::nir::Span;
use crate::workspace::{Arch, Target};

/// What lowering reads of a kernel's interface: its hash, the names of its exports in export
/// order, and the stack its interrupt functions may use.
pub struct Interface {
    pub hash: Vec<u8>,
    pub exports: Vec<String>,
    pub interrupt_stack: u64,
}

/// How an image takes part in loading: as a kernel exporting the load ABI, as a bundle built
/// against a kernel's interface, or not at all.
pub enum Linking {
    Image,
    Kernel(Rc<Interface>),
    Bundle,
}

/// The symbol of the export table, and the offsets in it of the line tables' address, the load
/// window's table, the window's next address, the export count and the first export's address.
pub const EXPORTS: &str = "_NSexports";
const EXPORT_LINES: u32 = 40;
const EXPORT_WINDOW: u32 = 56;
const EXPORT_NEXT: usize = 64;
const EXPORT_COUNT: usize = 88;
const EXPORT_FIRST: u32 = 96;

/// The load window's tables, and the window's first address: on x86_64 the table of 1 GiB
/// entries to which the entry root's top-level entry 256 points, the window starting at that
/// entry's addresses; on AArch64 the top-level table `TTBR1_EL1` holds, whose first entry points
/// to the table of 1 GiB entries following it, the window starting at the first address
/// `TTBR1_EL1` translates.
const WINDOW: &str = "_NSload_window";
const X64_WINDOW: u64 = 0xFFFF_8000_0000_0000;
const A64_WINDOW: u64 = 0xFFFF_0000_0000_0000;
pub const WINDOW_ENTRY: usize = 256;

/// The `TCR_EL1` and `TTBR1_EL1` system registers, and the translation control's fields for
/// `TTBR1_EL1`: its region size, walks disabled, cacheability, shareability and granule, which
/// the window sets to a 48-bit region walked with write-back, inner-shareable 4 KiB tables.
const TCR_EL1: u16 = 0x4102;
const TTBR1_EL1: u16 = 0x4101;
const TCR_HIGH_FIELDS: i64 = 0xFFBF_0000;
const TCR_HIGH: i64 = 0xB510_0000;

/// The bits of the window's entries the loader writes, by kind: a table, a page of code, of
/// read-only data and of writable data. On x86_64 present, writable and execute-disable; on
/// AArch64 a table or page descriptor, the access flag, inner-shareable normal write-back memory,
/// read-only and execute-never for EL1 and for EL0.
const X64_ENTRIES: [u64; 4] = [0x1, 0x8000_0000_0000_0001, 0x8000_0000_0000_0003, 0x3];
const A64_PAGE: u64 = 0x3 | 0x400 | 0x300 | (2 << 2) | 1 << 54;
const A64_ENTRIES: [u64; 4] = [
    A64_PAGE | 0x80,
    A64_PAGE | 0x80 | 1 << 53,
    A64_PAGE | 1 << 53,
    0x3,
];

/// The bytes of the stack a gate's own frame and calls use within the `stack N` of its function
/// type; the function it calls fits in the rest.
pub const GATE_STACK: u64 = 1_024;

/// The symbol of a bundle's own line tables.
pub const BUNDLE_LINES: &str = lines::BUNDLE_SYMBOL;

/// The prefix of the symbol of a loaded registry's table record, before its canonical path.
const RECORD: &str = "_NSloaded_";

/// The support routines a kernel exports: every one the lowered code of a kernel calls.
const ROUTINES: [Routine; 53] = [
    Routine::Copy,
    Routine::Fill,
    Routine::Compare,
    Routine::Alloc,
    Routine::AllocIn,
    Routine::Free,
    Routine::RecordAlloc,
    Routine::RecordFree,
    Routine::RecordResize,
    Routine::Split,
    Routine::Close,
    Routine::Charged,
    Routine::Limit,
    Routine::TaskEnv,
    Routine::TaskEnvFree,
    Routine::Lift,
    Routine::Hold,
    Routine::Unhold,
    Routine::Rehome,
    Routine::MintRoot,
    Routine::Derive,
    Routine::Revoke,
    Routine::OutboxMint,
    Routine::OutboxRetire,
    Routine::OutboxClaim,
    Routine::KindMint,
    Routine::KindPin,
    Routine::KindDerive,
    Routine::KindRevoke,
    Routine::KindClose,
    Routine::TrapRaise,
    Routine::TrapReport,
    Routine::Reacquired,
    Routine::Settle,
    Routine::ReportScalar,
    Routine::ReportText,
    Routine::WriteErr,
    Routine::OverflowRaise,
    Routine::Stop,
    Routine::Spawn,
    Routine::Wake,
    Routine::Park,
    Routine::RegionOpen,
    Routine::Cancel,
    Routine::Origin,
    Routine::CownRelease,
    Routine::Lock,
    Routine::Unlock,
    Routine::ClockNow,
    Routine::IommuFlush,
    Routine::Extended,
    Routine::Ready,
    Routine::Enqueue,
];

/// The names of the exports of a kernel of a target whose final closure declares the loaded
/// registries of these canonical paths, in export order.
pub fn export_names(target: Target, registries: &[String]) -> Vec<String> {
    let arch = target.arch().expect("a machine target's architecture");
    let mut names = vec![runtime::STATE.to_owned(), lines::SYMBOL.to_owned()];
    names.extend(ROUTINES.iter().map(|routine| routine.name().to_owned()));
    names.extend(
        machine::shared_data(arch)
            .iter()
            .map(|data| data.name.to_owned()),
    );
    names.extend(registries.iter().map(|path| format!("{RECORD}{path}")));
    names
}

/// The form in which loading compares a key or unique field of a type: 0 for `Str`, otherwise
/// the integer's width in bytes, with `0x80` set for a signed one.
fn form(ty: &Ty) -> u64 {
    let Ty::Scalar(scalar) = ty else {
        return 0;
    };
    match scalar {
        Scalar::U8 | Scalar::Bool => 1,
        Scalar::U16 => 2,
        Scalar::U32 | Scalar::Char => 4,
        Scalar::I8 => 0x81,
        Scalar::I16 => 0x82,
        Scalar::I32 => 0x84,
        Scalar::I64 | Scalar::Isize => 0x88,
        _ => 8,
    }
}

/// An entry a bundle adds to a loaded registry, by the symbols that hold it: the export name of
/// the registry's table record, the entry's data, its size, its key and unique fields as offset
/// and form, and the offset of each function field.
pub struct Contributed {
    pub record: String,
    pub entry: String,
    pub size: u64,
    pub key: (u64, u64),
    pub unique: Vec<(u64, u64)>,
    pub gates: Vec<u64>,
}

/// The load window's tables of an architecture.
fn window_tables(arch: Arch) -> Reserved {
    Reserved {
        name: WINDOW,
        size: match arch {
            Arch::Amd64 => 4_096,
            Arch::Aarch64 => 8_192,
        },
        align: 4_096,
    }
}

impl Lowering<'_, '_> {
    /// Whether the image is a bundle, importing the load ABI.
    pub fn imports(&self) -> bool {
        matches!(self.linking, Linking::Bundle)
    }

    /// Whether the image is a kernel exporting the load ABI.
    pub fn exports(&self) -> bool {
        matches!(self.linking, Linking::Kernel(_))
    }

    /// The stack a kernel's interface lets interrupt functions use, zero for another image.
    pub fn interface_interrupt_stack(&self) -> u64 {
        match &self.linking {
            Linking::Kernel(interface) => interface.interrupt_stack,
            _ => 0,
        }
    }

    /// The address of the export table, zero in an image that exports none.
    pub fn export_table(&mut self, fx: &mut super::fx::Fx) -> cranelift_codegen::ir::Value {
        match self.exports() {
            true => {
                let id = self
                    .module
                    .declare_data(EXPORTS, Linkage::Local, true, false)
                    .expect("the export table");
                self.data_address(fx, id)
            }
            false => fx.ptr(0),
        }
    }

    /// Defines a kernel's export table over every export its interface names, the loaded
    /// registries of its final closure being `registries`.
    pub fn export_abi(&mut self, registries: &[ItemId]) {
        let Linking::Kernel(interface) = &self.linking else {
            return;
        };
        let interface = interface.clone();
        let arch = self.ck.db.target.arch().expect("a machine target");
        let shared = machine::shared_data(arch);
        let mut functions = Vec::new();
        let window = self.reserved_data(window_tables(arch));
        let (window_table, base) = match arch {
            Arch::Amd64 => (0, X64_WINDOW),
            Arch::Aarch64 => (4_096, A64_WINDOW),
        };
        let mut data = vec![
            (
                EXPORT_LINES,
                self.lines.table(&mut self.module, &self.linking),
                0,
            ),
            (EXPORT_WINDOW, window, window_table),
        ];
        for (index, name) in interface.exports.iter().enumerate() {
            let at = EXPORT_FIRST + 8 * index as u32;
            if let Some(routine) = ROUTINES.iter().find(|routine| routine.name() == name) {
                functions.push((at, self.routine(*routine)));
                continue;
            }
            let id = if name == runtime::STATE {
                self.state_data()
            } else if name == lines::SYMBOL {
                self.lines.table(&mut self.module, &self.linking)
            } else if let Some(reserved) = shared.iter().find(|data| data.name == name) {
                self.reserved_data(*reserved)
            } else {
                let registry = registries
                    .iter()
                    .find(|registry| {
                        format!("{RECORD}{}", self.ck.db.canonical(**registry)) == *name
                    })
                    .unwrap_or_else(|| panic!("the export {name}"));
                self.loaded_record(*registry)
            };
            data.push((at, id, 0));
        }
        let mut bytes = interface.hash.clone();
        bytes.resize(EXPORT_FIRST as usize, 0);
        bytes[EXPORT_NEXT..EXPORT_NEXT + 8].copy_from_slice(&base.to_le_bytes());
        bytes[EXPORT_COUNT..EXPORT_COUNT + 8]
            .copy_from_slice(&(interface.exports.len() as u64).to_le_bytes());
        bytes.resize(EXPORT_FIRST as usize + 8 * interface.exports.len(), 0);
        let mut description = DataDescription::new();
        description.define(bytes.into_boxed_slice());
        description.set_align(8);
        for (at, function) in functions {
            let reference = self.module.declare_func_in_data(function, &mut description);
            description.write_function_addr(at, reference);
        }
        for (at, id, addend) in data {
            let reference = self.module.declare_data_in_data(id, &mut description);
            description.write_data_addr(at, reference, addend);
        }
        let id = self
            .module
            .declare_data(EXPORTS, Linkage::Local, true, false)
            .expect("the export table");
        self.module
            .define_data(id, &description)
            .expect("the export table");
    }

    /// The table record of a loaded registry, declared once: a bundle imports it, any other
    /// image defines it holding the registry's table as its first version, with the gate of each
    /// function field of the entry type.
    pub fn loaded_record(&mut self, registry: ItemId) -> DataId {
        if let Some(id) = self.data.records.get(&registry) {
            return *id;
        }
        let name = format!("{RECORD}{}", self.ck.db.canonical(registry));
        let id = match self.imports() {
            true => self
                .module
                .declare_data(&name, Linkage::Import, true, false)
                .expect("a table record"),
            false => {
                let id = self
                    .module
                    .declare_data(&name, Linkage::Local, true, false)
                    .expect("a table record");
                let table = self.table(registry);
                let gates = self.gates(registry);
                let mut bytes = vec![0; 16 + 16 * gates.len()];
                bytes[8..16].copy_from_slice(&(gates.len() as u64).to_le_bytes());
                for (index, (offset, _)) in gates.iter().enumerate() {
                    let at = 16 + 16 * index;
                    bytes[at..at + 8].copy_from_slice(&offset.to_le_bytes());
                }
                let mut description = DataDescription::new();
                description.define(bytes.into_boxed_slice());
                description.set_align(8);
                let reference = self.module.declare_data_in_data(table, &mut description);
                description.write_data_addr(0, reference, 0);
                for (index, (_, gate)) in gates.iter().enumerate() {
                    let reference = self.module.declare_func_in_data(*gate, &mut description);
                    description.write_function_addr(24 + 16 * index as u32, reference);
                }
                self.module
                    .define_data(id, &description)
                    .expect("a table record");
                id
            }
        };
        self.data.records.insert(registry, id);
        id
    }

    /// The offset of each function field of a loaded registry's entry type, with the gate of its
    /// type.
    fn gates(&mut self, registry: ItemId) -> Vec<(u64, FuncId)> {
        let SigKind::Registry { entry } = &self.ck.sig(registry).kind else {
            panic!("a registry's entry type");
        };
        let entry = entry.clone();
        let Ty::Item(structure, _) = &entry else {
            panic!("a struct entry type");
        };
        let SigKind::Struct { fields } = &self.ck.sig(*structure).kind else {
            panic!("a struct entry type");
        };
        let mut gates = Vec::new();
        for field in fields.clone() {
            let Ty::Fn(fn_ty) = &field.ty else {
                continue;
            };
            let offset = self
                .layouts
                .field(self.ck, &entry, None, &field.name)
                .offset;
            gates.push((offset, self.gate(fn_ty, registry)));
        }
        gates
    }

    /// The gate of a function type, called with a gate record as its environment. It takes a
    /// holding pinning the target its record names, trapping with `trap.stale-handle` when the
    /// record names none, calls the target's code, then releases the pin. A `stack N` of the type
    /// demands it of the gate with the target bounded by `N` less `GATE_STACK`, reported at the
    /// registry.
    fn gate(&mut self, fn_ty: &FnTy, registry: ItemId) -> FuncId {
        let glue = Glue::Gate(fn_ty.clone());
        let new = !self.glues.contains_key(&glue);
        let abi = self.fn_abi(fn_ty);
        let signature = abi.signature();
        let bound = fn_ty
            .limits
            .stack
            .map(|stack| stack.saturating_sub(GATE_STACK));
        let id = self.glue_function(glue, signature.clone(), |lo, fx| {
            let params = fx.entry();
            let record = params[0];
            let pin = fx.ptr(holdings::kind::PIN as u64);
            let zero = fx.ptr(0);
            let holding = lo.call_routine(fx, Routine::Hold, &[pin, zero])[0];
            let found = fx.repeat(&[zero], |fx, _| {
                let target = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), record);
                let stale = fx.b.ins().icmp_imm_s(IntCC::Equal, target, 0);
                fx.when(stale, |fx| {
                    lo.call_routine(fx, Routine::Unhold, &[holding]);
                    report::raise_kind(lo, fx, "trap.stale-handle")
                });
                let pins = fx.offset(target, 8);
                let one = fx.ptr(1);
                fetch_add(fx, pins, one);
                let again = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), record);
                let moved = fx.b.ins().icmp(IntCC::NotEqual, again, target);
                fx.when(moved, |fx| {
                    let less = fx.int(PTR, -1);
                    fetch_add(fx, pins, less);
                });
                (moved, vec![target])
            });
            let pins = fx.offset(found[0], 8);
            fx.store(pins, holding, holdings::holding::PAYLOAD);
            let code = fx.load(PTR, found[0], 0);
            let mut args = params.clone();
            args[0] = fx.ptr(0);
            let signature = fx.b.import_signature(signature);
            let inst = fx.b.ins().call_indirect(signature, code, &args);
            fx.sites.push((
                inst,
                Site {
                    kind: stack::Kind::Bounded(bound),
                    at: None,
                },
            ));
            let results = fx.b.inst_results(inst).to_vec();
            lo.call_routine(fx, Routine::Unhold, &[holding]);
            let less = fx.int(PTR, -1);
            fetch_add(fx, pins, less);
            fx.b.ins().return_(&results);
        });
        if let (true, Some(stack)) = (new, fn_ty.limits.stack) {
            let span = Span::at(self.ck.db.item(registry).ast.token);
            self.demands.push(Demand {
                judged: Judged::Function(id),
                stack,
                report: Report::Fit(stack::At {
                    item: registry,
                    span,
                }),
            });
        }
        id
    }

    /// The entry a bundle's contribution adds to a loaded registry, lowering its value; a
    /// `stack N` of a function field's type demands `N` less `GATE_STACK` of its function,
    /// reported at the contribution.
    pub fn contribute(&mut self, contribution: ItemId, registry: ItemId) -> Contributed {
        let SigKind::Registry { entry } = &self.ck.sig(registry).kind else {
            panic!("a registry's entry type");
        };
        let entry = entry.clone();
        let crate::syntax::ast::ItemKind::Registry(declared) = &self.ck.db.ast(registry).kind
        else {
            panic!("a registry item");
        };
        let declared = declared.clone();
        let value = self
            .ck
            .const_value(contribution)
            .expect("a contribution's value");
        let data = self.const_data(contribution, &entry);
        let layout = self.layout(&entry);
        let field = |lo: &mut Self, name: &str| {
            let field = lo.layouts.field(lo.ck, &entry, None, name);
            (field.offset, form(&field.ty))
        };
        let key = field(self, &declared.key.text);
        let unique = declared
            .unique
            .iter()
            .map(|name| field(self, &name.text))
            .collect();
        let Val::Struct(_, values) = &value.value else {
            panic!("a contribution's struct value");
        };
        let Ty::Item(structure, _) = &entry else {
            panic!("a struct entry type");
        };
        let SigKind::Struct { fields } = &self.ck.sig(*structure).kind else {
            panic!("a struct entry type");
        };
        let fields = fields.clone();
        let mut gates = Vec::new();
        for (field_sig, field_value) in fields.iter().zip(values.iter()) {
            let (Ty::Fn(fn_ty), Val::Fn(function)) = (&field_sig.ty, field_value) else {
                continue;
            };
            let FnVal::Function(fn_ref) = &**function else {
                panic!("a closure in a contribution");
            };
            if let Some(stack) = fn_ty.limits.stack {
                let inner = self.callable_instance(fn_ref, true);
                let id = self.instance(Instance::Value(Box::new(inner)));
                let span = Span::at(self.ck.db.item(contribution).ast.token);
                self.demands.push(Demand {
                    judged: Judged::Function(id),
                    stack: stack.saturating_sub(GATE_STACK),
                    report: Report::Fit(stack::At {
                        item: contribution,
                        span,
                    }),
                });
            }
            let offset = self
                .layouts
                .field(self.ck, &entry, None, &field_sig.name)
                .offset;
            gates.push(offset);
        }
        Contributed {
            record: format!("{RECORD}{}", self.ck.db.canonical(registry)),
            entry: self.data_symbol(data),
            size: layout.size,
            key,
            unique,
            gates,
        }
    }

    /// Opens the load window to the running CPU of a kernel exporting the load ABI: on x86_64
    /// kernel writes honour read-only pages and execute-disable bits are enabled; on AArch64
    /// `TTBR1_EL1` walks the window's tables.
    pub fn open_window(&mut self, fx: &mut Fx) {
        if !self.exports() {
            return;
        }
        let arch = self.ck.db.target.arch().expect("a machine target");
        if arch == Arch::Amd64 {
            self.call_stub(fx, Stub::WriteProtect, &[]);
            return;
        }
        let window = self.reserved(fx, window_tables(arch));
        let table = fx.offset(window, 4_096);
        let entry = fx.b.ins().bor_imm_s(table, A64_ENTRIES[3] as i64);
        fx.store(entry, window, 0);
        self.call_stub(fx, Stub::WriteSysReg(TTBR1_EL1), &[window]);
        let control = self.call_stub(fx, Stub::ReadSysReg(TCR_EL1), &[])[0];
        let control = fx.b.ins().band_imm_s(control, !TCR_HIGH_FIELDS);
        let control = fx.b.ins().bor_imm_s(control, TCR_HIGH);
        self.call_stub(fx, Stub::WriteSysReg(TCR_EL1), &[control]);
        self.call_stub(fx, Stub::TlbiAll, &[]);
    }

    /// The window entry of a kind, `kind` being 0 for code, 1 for read-only data, 2 for
    /// writable data and 3 for a table, over the page or table at a physical address.
    pub fn window_entry(&mut self, fx: &mut Fx, physical: Value, kind: Value) -> Value {
        let entries = match self.ck.db.target.arch() {
            Some(Arch::Aarch64) => A64_ENTRIES,
            _ => X64_ENTRIES,
        };
        let mut bits = fx.ptr(entries[3]);
        for (index, entry) in entries.iter().enumerate().take(3) {
            let this = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, index as i64);
            let value = fx.ptr(*entry);
            bits = fx.b.ins().select(this, value, bits);
        }
        fx.b.ins().bor(physical, bits)
    }

    /// The entry-root top-level entry of the load window on x86_64: its table, writable.
    pub fn window_root_entry(&mut self) -> Option<(DataId, i64)> {
        if !self.exports() || self.ck.db.target.arch() != Some(Arch::Amd64) {
            return None;
        }
        Some((self.reserved_data(window_tables(Arch::Amd64)), 0x3))
    }

    /// The symbol of a data object of the object.
    fn data_symbol(&self, id: DataId) -> String {
        let declaration = self.module.declarations().get_data_decl(id);
        declaration.name.clone().expect("a named data object")
    }
}
