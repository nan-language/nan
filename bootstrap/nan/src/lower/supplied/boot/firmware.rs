//! The firmware services of the UEFI targets, reached through the system table the program
//! entry seam recorded and called under the firmware's convention on the boot stack: the
//! services `uefi_platform` implements over its calls, the end of boot services, after which the
//! image runs on its own entry root and descriptor tables, the hand-off to a loaded bare image,
//! and the time.

use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature, Value, condcodes::IntCC, types};
use cranelift_codegen::isa::CallConv;

use super::super::super::Lowering;
use super::super::super::body::Held;
use super::super::super::fx::{Fx, PTR};
use super::super::super::machine::Stub;
use super::super::super::runtime::{Routine, report, state};
use super::super::super::stack::{Kind, Site};
use super::super::Supply;
use super::super::hw::region;
use crate::workspace::Arch;

/// The offsets in the system table of its runtime services and its boot services; in the
/// runtime services of `GetTime`; in the boot services of `GetMemoryMap` and
/// `ExitBootServices`.
const RUNTIME_SERVICES: u64 = 88;
const BOOT_SERVICES: u64 = 96;
const GET_TIME: u64 = 24;
const GET_MEMORY_MAP: u64 = 56;
const EXIT_BOOT_SERVICES: u64 = 232;

/// The statuses `exit_services` and `GetTime` tell apart, an error's having its high bit set.
const EFI_SUCCESS: i64 = 0;
const EFI_BUFFER_TOO_SMALL: i64 = 0x8000_0000_0000_0005_u64 as i64;

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

/// `services.f(...)`: calls the function `f` of `uefi_platform` with the arguments after the
/// services, giving its result or forwarding its failure.
pub fn service(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let args = supply.params[1..].to_vec();
    let called = lo.platform_call(fx, supply.member(), &args);
    super::super::hw::forward(lo, fx, supply, called);
}

/// `services.free(take region)`: calls `uefi_platform::free` with the region's base and length.
pub fn free(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let held = supply.param(1).address();
    let base = fx.load(PTR, held, region::BASE);
    let len = fx.load(PTR, held, region::LEN);
    let called = lo.platform_call(fx, "free", &[Held::Scalar(base), Held::Scalar(len)]);
    super::super::hw::forward(lo, fx, supply, called);
}

/// `take m.enter(take image)`: hands the image to `uefi_platform::hand_off` with the machine's
/// root allocator; it never returns.
pub fn enter(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let machine = supply.param(0).address();
    let at = super::field(lo, supply, "alloc");
    let alloc = fx.offset(machine, at);
    lo.platform_call(fx, "hand_off", &[Held::Address(alloc), supply.param(1)]);
    fx.b.ins()
        .trap(cranelift_codegen::ir::TrapCode::user(1).expect("a trap code"));
}

/// The operations `uefi_platform` declares without a body: a call of a firmware function with
/// five arguments, the system table and image handle the entry seam recorded, a region from its
/// base and length, the content-blind copy of Untrusted bytes to an address, the cleaning of a
/// range from the data caches, the jump into a loaded bare image and a trap of exhausted memory.
pub fn platform(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let arch = lo.ck.db.target.arch();
    match supply.path.as_str() {
        "uefi_platform::call" => {
            let function = supply.param(0).scalar();
            let args: Vec<Value> = (1..6).map(|index| supply.param(index).scalar()).collect();
            let status = firmware_call(arch, fx, function, &args);
            supply.ret(lo, fx, Held::Scalar(status));
        }
        "uefi_platform::system_table" => {
            let table = system_table(lo, fx);
            supply.ret(lo, fx, Held::Scalar(table));
        }
        "uefi_platform::image_handle" => {
            let state_address = lo.state(fx);
            let image = fx.load(PTR, state_address, state::PLATFORM);
            supply.ret(lo, fx, Held::Scalar(image));
        }
        "uefi_platform::region" => {
            let held = fx.slot(16, 8);
            fx.store(supply.param(0).scalar(), held, region::BASE);
            fx.store(supply.param(1).scalar(), held, region::LEN);
            let result = lo.held_at(fx, &supply.abi.result, held);
            supply.ret(lo, fx, result);
        }
        "uefi_platform::place" => {
            let contents = supply.param(0).address();
            let start = fx.load(PTR, contents, 0);
            let len = fx.load(PTR, contents, 8);
            let at = supply.param(1).scalar();
            lo.call_routine(fx, Routine::Copy, &[at, start, len]);
            supply.done(lo, fx);
        }
        "uefi_platform::clean" => {
            if arch == Some(Arch::Aarch64) {
                let range = [supply.param(0).scalar(), supply.param(1).scalar()];
                lo.call_stub(fx, Stub::CleanCache, &range);
            }
            supply.done(lo, fx);
        }
        "uefi_platform::jump" => {
            let [entry, info, tree, trampoline] =
                [0, 1, 2, 3].map(|index| supply.param(index).scalar());
            lo.enter_image(fx, entry, info, tree, trampoline);
        }
        "uefi_platform::exhausted" => report::raise_kind(lo, fx, "trap.exhausted"),
        path => panic!("no lowering of {path}"),
    }
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
