//! The firmware services of `x86_64-uefi`, reached through the system table the program entry
//! seam recorded and called under the Microsoft x64 convention on the boot stack: console
//! output, files of the volume the image was loaded from, the end of boot services, after which
//! the image runs on its own entry root and descriptor tables, and the time.

use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature, Value, condcodes::IntCC, types};
use cranelift_codegen::isa::CallConv;
use cranelift_frontend::Variable;

use super::super::super::Lowering;
use super::super::super::body::Held;
use super::super::super::fx::{Fx, PTR, unaligned};
use super::super::super::machine::Stub;
use super::super::super::runtime::{report, state};
use super::super::super::stack::{Kind, Site};
use super::super::mem::vector::{self, Element};
use super::super::{Supply, sys};
use crate::workspace::Arch;

/// The offsets in the system table of its console-output protocol, its runtime services and its
/// boot services; in the console-output protocol of its string output; in the runtime services
/// of `GetTime`; in the boot services of `GetMemoryMap`, `HandleProtocol` and
/// `ExitBootServices`.
const CONSOLE_OUT: u64 = 64;
const RUNTIME_SERVICES: u64 = 88;
const BOOT_SERVICES: u64 = 96;
const GET_TIME: u64 = 24;
const OUTPUT_STRING: u64 = 8;
const GET_MEMORY_MAP: u64 = 56;
const HANDLE_PROTOCOL: u64 = 152;
const EXIT_BOOT_SERVICES: u64 = 232;

/// The offsets of a loaded image protocol's device handle, of a simple file system protocol's
/// `OpenVolume`, and of a file protocol's functions.
const DEVICE_HANDLE: u64 = 24;
const OPEN_VOLUME: u64 = 8;
mod file {
    pub const OPEN: u64 = 8;
    pub const CLOSE: u64 = 16;
    pub const READ: u64 = 32;
    pub const GET_POSITION: u64 = 48;
    pub const SET_POSITION: u64 = 56;
}

/// The protocols the services open, by GUID.
const LOADED_IMAGE: [u8; 16] = [
    0xA1, 0x31, 0x1B, 0x5B, 0x62, 0x95, 0xD2, 0x11, 0x8E, 0x3F, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B,
];
const SIMPLE_FILE_SYSTEM: [u8; 16] = [
    0x22, 0x5B, 0x4E, 0x96, 0x59, 0x64, 0xD2, 0x11, 0x8E, 0x39, 0x00, 0xA0, 0xC9, 0x69, 0x72, 0x3B,
];

/// The statuses the services tell apart, an error's having its high bit set; the mode opening a
/// file for reading; and the file position past its end.
const EFI_SUCCESS: i64 = 0;
const EFI_BUFFER_TOO_SMALL: i64 = 0x8000_0000_0000_0005_u64 as i64;
const EFI_NOT_FOUND: i64 = 0x8000_0000_0000_000E_u64 as i64;
const EFI_OUT_OF_RESOURCES: i64 = 0x8000_0000_0000_0009_u64 as i64;
const READ_MODE: u64 = 1;
const END_OF_FILE: i64 = -1;

/// The descriptors of slack a memory map buffer holds for the entries the firmware adds before
/// the map is read into it.
const MAP_SLACK: i64 = 8;

/// `m.with_services(body)`: calls `body` with the firmware's services, a view, and returns its
/// result or forwards its failure.
pub fn with_services(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let body_ty = super::super::mem::lists::fn_param(supply, 1);
    let called = lo.call_value(fx, &body_ty, supply.param(1).address(), &[Held::Empty]);
    super::super::hw::forward(lo, fx, supply, called);
}

/// `services.write_console(text)`: writes `text` to the firmware's console output in UTF-16,
/// each line feed preceded by a carriage return.
pub fn write_console(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let arch = lo.ck.db.target.arch();
    let text = supply.param(1).address();
    let start = fx.load(PTR, text, 0);
    let len = fx.load(PTR, text, 8);
    let units = utf16(lo, fx, supply, start, len, |fx, byte, written, units| {
        let newline = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'\n'));
        fx.when(newline, |fx| {
            let carriage = fx.int(types::I16, i64::from(b'\r'));
            put_unit(fx, units, written, carriage);
        });
        put_unit(fx, units, written, byte);
    });
    let table = system_table(lo, fx);
    let conout = fx.load(PTR, table, CONSOLE_OUT);
    let output = fx.load(PTR, conout, OUTPUT_STRING);
    let status = firmware_call(arch, fx, output, &[conout, units]);
    lo.record_free(fx, units);
    fail_unless(lo, fx, supply, status, |_, _| {});
    supply.done(lo, fx);
}

/// `services.read_file(path, alloc)`: reads the file `path` names from the root directory of
/// the volume the image was loaded from, through the loaded image's device and its simple file
/// system, into bytes drawn through `alloc`.
pub fn read_file(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let arch = lo.ck.db.target.arch();
    let path = supply.param(1).address();
    let cell = fx.load(PTR, supply.param(2).address(), 0);
    let table = system_table(lo, fx);
    let services = fx.load(PTR, table, BOOT_SERVICES);
    let handle_protocol = fx.load(PTR, services, HANDLE_PROTOCOL);
    let found = fx.slot(8, 8);
    let image = {
        let state_address = lo.state(fx);
        fx.load(PTR, state_address, state::PLATFORM)
    };
    let loaded_image = guid(lo, fx, &LOADED_IMAGE);
    let status = firmware_call(arch, fx, handle_protocol, &[image, loaded_image, found]);
    fail_unless(lo, fx, supply, status, |_, _| {});
    let loaded = fx.load(PTR, found, 0);
    let device = fx.load(PTR, loaded, DEVICE_HANDLE);
    let file_system = guid(lo, fx, &SIMPLE_FILE_SYSTEM);
    let status = firmware_call(arch, fx, handle_protocol, &[device, file_system, found]);
    fail_unless(lo, fx, supply, status, |_, _| {});
    let volume = fx.load(PTR, found, 0);
    let open_volume = fx.load(PTR, volume, OPEN_VOLUME);
    let status = firmware_call(arch, fx, open_volume, &[volume, found]);
    fail_unless(lo, fx, supply, status, |_, _| {});
    let root = fx.load(PTR, found, 0);
    let start = fx.load(PTR, path, 0);
    let len = fx.load(PTR, path, 8);
    let name = utf16(lo, fx, supply, start, len, |fx, byte, written, units| {
        let slash = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'/'));
        let backslash = fx.int(types::I16, i64::from(b'\\'));
        let unit = fx.b.ins().select(slash, backslash, byte);
        put_unit(fx, units, written, unit);
    });
    let open = fx.load(PTR, root, file::OPEN);
    let mode = fx.ptr(READ_MODE);
    let attributes = fx.ptr(0);
    let status = firmware_call(arch, fx, open, &[root, found, name, mode, attributes]);
    lo.record_free(fx, name);
    fail_unless(lo, fx, supply, status, |_, fx| close(arch, fx, &[root]));
    let file = fx.load(PTR, found, 0);
    let set_position = fx.load(PTR, file, file::SET_POSITION);
    let end = fx.int(PTR, END_OF_FILE);
    firmware_call(arch, fx, set_position, &[file, end]);
    let get_position = fx.load(PTR, file, file::GET_POSITION);
    firmware_call(arch, fx, get_position, &[file, found]);
    let size = fx.load(PTR, found, 0);
    let zero = fx.ptr(0);
    firmware_call(arch, fx, set_position, &[file, zero]);
    let bytes = supply.sret.expect("the bytes' address");
    let element = Element { size: 1, align: 1 };
    vector::empty(fx, bytes, cell, element);
    let room = vector::reserve(lo, fx, bytes, size, element);
    let short = fx.b.ins().icmp_imm_s(IntCC::Equal, room, 0);
    fx.when(short, |fx| {
        close(arch, fx, &[file, root]);
        sys::fail_with(lo, fx, supply, "OutOfMemory");
    });
    let data = fx.load(PTR, bytes, vector::DATA);
    let read = fx.load(PTR, file, file::READ);
    let filled = fx.repeat(&[zero], |fx, state| {
        let offset = state[0];
        let left = fx.b.ins().isub(size, offset);
        fx.store(left, found, 0);
        let at = fx.b.ins().iadd(data, offset);
        let status = firmware_call(arch, fx, read, &[file, found, at]);
        fail_unless(lo, fx, supply, status, |lo, fx| {
            close(arch, fx, &[file, root]);
            vector::free_storage(lo, fx, bytes);
        });
        let got = fx.load(PTR, found, 0);
        let offset = fx.b.ins().iadd(offset, got);
        let more = fx.b.ins().icmp(IntCC::UnsignedLessThan, offset, size);
        let progress = fx.b.ins().icmp_imm_s(IntCC::NotEqual, got, 0);
        (fx.b.ins().band(more, progress), vec![offset])
    })[0];
    fx.store(filled, bytes, vector::LEN);
    close(arch, fx, &[file, root]);
    supply.ret(lo, fx, Held::Address(bytes));
}

/// `take m.exit_services()`: reads the firmware's final memory map into a runtime record the
/// runtime state keeps, in a record grown while the map does not fit it, and ends boot services
/// with its key, reading the map again while they refuse; then, with maskable interrupts
/// disabled, extends the entry root over usable memory above 4 GiB and installs it, on x86_64
/// with the boot CPU's descriptor tables and AVX enabled when the CPU has it, on AArch64 with the
/// memory attributes and translation control the entry root needs; and gives the machine's
/// `boot::Kernel`, which holds what the `boot::Machine` held.
pub fn exit_services(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let arch = lo.ck.db.target.arch();
    let machine = supply.param(0).address();
    let table = system_table(lo, fx);
    let services = fx.load(PTR, table, BOOT_SERVICES);
    let get_map = fx.load(PTR, services, GET_MEMORY_MAP);
    let exit = fx.load(PTR, services, EXIT_BOOT_SERVICES);
    let size = fx.slot(8, 8);
    let key = fx.slot(8, 8);
    let stride = fx.slot(8, 8);
    let version = fx.slot(8, 8);
    let image = {
        let state_address = lo.state(fx);
        fx.load(PTR, state_address, state::PLATFORM)
    };
    let zero = fx.ptr(0);
    let ended = fx.repeat(&[zero, zero], |fx, held| {
        let (map, room) = (held[0], held[1]);
        fx.store(room, size, 0);
        let status = firmware_call(arch, fx, get_map, &[size, map, key, stride, version]);
        let short =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, status, EFI_BUFFER_TOO_SMALL);
        let next = fx.choose_in(
            lo,
            short,
            &[PTR, PTR, types::I8],
            |lo, fx| {
                lo.record_free(fx, map);
                let needed = fx.load(PTR, size, 0);
                let slack = fx.load(PTR, stride, 0);
                let slack = fx.b.ins().imul_imm_s(slack, MAP_SLACK);
                let room = fx.b.ins().iadd(needed, slack);
                let map = lo.record_alloc(fx, room);
                let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, map, 0);
                fx.when(missing, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
                vec![map, room, fx.int(types::I8, 1)]
            },
            |_, fx| {
                let current = fx.load(PTR, key, 0);
                let status = firmware_call(arch, fx, exit, &[image, current]);
                let refused = fx.b.ins().icmp_imm_s(IntCC::NotEqual, status, EFI_SUCCESS);
                vec![map, room, refused]
            },
        );
        (next[2], next[..2].to_vec())
    });
    let map = ended[0];
    lo.call_stub(fx, Stub::Cli, &[]);
    let state_address = lo.state(fx);
    fx.store(map, state_address, state::MEMORY_MAP);
    let used = fx.load(PTR, size, 0);
    fx.store(used, state_address, state::MAP_SIZE);
    let described = fx.load(PTR, stride, 0);
    fx.store(described, state_address, state::MAP_STRIDE);
    lo.extend_entry_root(fx);
    let root = lo.entry_root();
    let root = lo.data_address(fx, root);
    match arch {
        Some(Arch::Aarch64) => {
            lo.call_stub(fx, Stub::Cli, &[]);
            lo.call_stub(fx, Stub::EnterRoot, &[root]);
        }
        _ => {
            lo.call_stub(fx, Stub::WriteCr3, &[root]);
            let boot = lo.cpu_records(fx);
            lo.load_cpu_tables(fx, boot);
            lo.call_stub(fx, Stub::EnableAvx, &[]);
        }
    }
    let kernel = supply.sret.expect("the kernel's address");
    let grant = supply.owner.clone().expect("the machine's root grant");
    let layout = lo.layout(&grant);
    lo.copy(fx, kernel, machine, layout.size, layout.align);
    supply.ret(lo, fx, Held::Address(kernel));
}

/// The firmware's time from `GetTime`: whether it gave one, its year, month, day, hour, minute
/// and second, and its nanoseconds.
pub fn time(lo: &mut Lowering, fx: &mut Fx) -> (Value, [Value; 6], Value) {
    let arch = lo.ck.db.target.arch();
    let table = system_table(lo, fx);
    let services = fx.load(PTR, table, RUNTIME_SERVICES);
    let get_time = fx.load(PTR, services, GET_TIME);
    let time = fx.slot(16, 8);
    let zero = fx.ptr(0);
    fx.store(zero, time, 0);
    fx.store(zero, time, 8);
    let status = firmware_call(arch, fx, get_time, &[time, zero]);
    let valid = fx.b.ins().icmp_imm_s(IntCC::Equal, status, EFI_SUCCESS);
    let year = fx.load(types::I16, time, 0);
    let year = fx.b.ins().uextend(PTR, year);
    let mut fields = [year; 6];
    for (index, offset) in [2, 3, 4, 5, 6].into_iter().enumerate() {
        let byte = fx.load(types::I8, time, offset);
        fields[index + 1] = fx.b.ins().uextend(PTR, byte);
    }
    let nanoseconds = fx.load(types::I32, time, 8);
    let nanoseconds = fx.b.ins().uextend(PTR, nanoseconds);
    (valid, fields, nanoseconds)
}

/// The system table the program entry seam recorded.
fn system_table(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let state_address = lo.state(fx);
    fx.load(PTR, state_address, state::PLATFORM_TABLE)
}

/// The address of a GUID in the image's read-only data.
fn guid(lo: &mut Lowering, fx: &mut Fx, bytes: &[u8; 16]) -> Value {
    let view = lo.view_literal(fx, bytes).address();
    fx.load(PTR, view, 0)
}

/// A NUL-terminated UTF-16 text in a runtime record, built from the UTF-8 text of `len` bytes at
/// `start`: a code point above U+FFFF as its surrogate pair, any other as the code unit given to
/// `put`. The service fails with `OutOfMemory` when no record can be drawn.
fn utf16(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    start: Value,
    len: Value,
    put: impl FnOnce(&mut Fx, Value, Variable, Value),
) -> Value {
    let doubled = fx.b.ins().imul_imm_s(len, 4);
    let room = fx.b.ins().iadd_imm_s(doubled, 4);
    let units = lo.record_alloc(fx, room);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, units, 0);
    fx.when(missing, |fx| sys::fail_with(lo, fx, supply, "OutOfMemory"));
    let zero = fx.ptr(0);
    let written = fx.var(PTR, zero);
    let any = fx.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
    fx.when(any, |fx| {
        fx.repeat(&[zero], |fx, state| {
            let place = fx.b.ins().iadd(start, state[0]);
            let first = fx.load(types::I8, place, 0);
            let first = fx.b.ins().uextend(PTR, first);
            let (point, step) = report::decode_utf8(fx, place, first);
            let paired =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, point, 0x1_0000);
            fx.choose(
                paired,
                &[],
                |fx| {
                    let offset = fx.b.ins().iadd_imm_s(point, -0x1_0000);
                    let high = fx.b.ins().ushr_imm_s(offset, 10);
                    let high = fx.b.ins().iadd_imm_s(high, 0xD800);
                    let low = fx.b.ins().band_imm_s(offset, 0x3FF);
                    let low = fx.b.ins().iadd_imm_s(low, 0xDC00);
                    for unit in [high, low] {
                        let unit = fx.b.ins().ireduce(types::I16, unit);
                        put_unit(fx, units, written, unit);
                    }
                    Vec::new()
                },
                |fx| {
                    let unit = fx.b.ins().ireduce(types::I16, point);
                    put(fx, unit, written, units);
                    Vec::new()
                },
            );
            let next = fx.b.ins().iadd(state[0], step);
            let more = fx.b.ins().icmp(IntCC::UnsignedLessThan, next, len);
            (more, vec![next])
        });
    });
    let nul = fx.int(types::I16, 0);
    put_unit(fx, units, written, nul);
    units
}

/// Appends a UTF-16 code unit to a buffer, advancing the count held in a variable.
fn put_unit(fx: &mut Fx, units: Value, written: Variable, unit: Value) {
    let at = fx.b.use_var(written);
    let offset = fx.b.ins().imul_imm_s(at, 2);
    let slot = fx.b.ins().iadd(units, offset);
    fx.b.ins().store(unaligned(), unit, slot, 0);
    let next = fx.b.ins().iadd_imm_s(at, 1);
    fx.b.def_var(written, next);
}

/// Closes file protocol handles.
fn close(arch: Option<Arch>, fx: &mut Fx, handles: &[Value]) {
    for handle in handles {
        let close = fx.load(PTR, *handle, file::CLOSE);
        firmware_call(arch, fx, close, &[*handle]);
    }
}

/// Fails the service with the error a status names when it is an error, after `cleanup`:
/// `NotFound` for `EFI_NOT_FOUND`, `OutOfMemory` for `EFI_OUT_OF_RESOURCES` and `DeviceError` for
/// any other error.
fn fail_unless(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    status: Value,
    cleanup: impl FnOnce(&mut Lowering, &mut Fx),
) {
    let failed = fx.b.ins().icmp_imm_s(IntCC::SignedLessThan, status, 0);
    fx.when(failed, |fx| {
        cleanup(lo, fx);
        let not_found = fx.b.ins().icmp_imm_s(IntCC::Equal, status, EFI_NOT_FOUND);
        fx.when(not_found, |fx| sys::fail_with(lo, fx, supply, "NotFound"));
        let exhausted =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, status, EFI_OUT_OF_RESOURCES);
        fx.when(exhausted, |fx| {
            sys::fail_with(lo, fx, supply, "OutOfMemory")
        });
        sys::fail_with(lo, fx, supply, "DeviceError");
    });
}

/// Calls a firmware function at an address under the firmware's convention, giving its status:
/// the Microsoft x64 convention on x86_64 and the Procedure Call Standard on AArch64. The
/// firmware runs on the boot stack, outside the task-stack accounting, as a stub does.
fn firmware_call(arch: Option<Arch>, fx: &mut Fx, code: Value, args: &[Value]) -> Value {
    let convention = match arch {
        Some(Arch::Aarch64) => CallConv::SystemV,
        _ => CallConv::WindowsFastcall,
    };
    let mut signature = Signature::new(convention);
    signature
        .params
        .extend(args.iter().map(|_| AbiParam::new(PTR)));
    signature.returns.push(AbiParam::new(PTR));
    let signature = fx.b.import_signature(signature);
    let call = fx.b.ins().call_indirect(signature, code, args);
    fx.sites.push((
        call,
        Site {
            kind: Kind::Stub,
            at: None,
        },
    ));
    fx.b.inst_results(call)[0]
}
