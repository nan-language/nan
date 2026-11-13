//! The Windows targets: everything Windows-specific. The program entry seam, the image's entry
//! point; the executor threads, one per processor of the process's affinity set, which all wait
//! on the process's I/O completion port; termination requests, which the console's control
//! handler passes to them through that port; stack overflows, which a vectored exception handler
//! receives; and the support routines' calls into the system, each through the import address
//! table's word of a `kernel32.dll` function.

mod requests;

pub use requests::{associate, cancel_request, define_request, kind as request};

use cranelift_codegen::ir::{
    AbiParam, Inst, InstBuilder, MemFlagsData, Signature, TrapCode, Type, Value, condcodes::IntCC,
    types,
};
use cranelift_codegen::isa::CallConv;
use cranelift_module::{FuncId, Linkage, Module};

use super::super::fx::{Fx, PTR};
use super::super::machine::Stub;
use super::super::runtime::system::{self, unavailable_if, unavailable_unless};
use super::super::runtime::tasks::{self, operation, task, thread};
use super::super::runtime::{Routine, fetch_add, state, timers};
use super::super::stack::{Kind, Site};
use super::super::{Lowering, Program, lines, test_image};
use super::Context;
use crate::image::IMPORT_PREFIX;
use crate::workspace::Arch;

/// The symbol of the program entry seam, the image's entry point.
pub const ENTRY: &str = "_NSentry";

/// The flags of `HeapAlloc` giving zeroed memory, of `VirtualAlloc` reserving and committing
/// memory, of `VirtualFree` releasing a whole reservation, and the protections of memory that
/// can be read and written and of memory that cannot be accessed.
const HEAP_ZERO_MEMORY: i64 = 8;
const MEM_COMMIT_RESERVE: i64 = 0x3000;
const MEM_RELEASE: i64 = 0x8000;
const PAGE_READWRITE: i64 = 4;
const PAGE_NOACCESS: i64 = 1;

/// The pseudo handle `INVALID_HANDLE_VALUE`, which makes `CreateIoCompletionPort` create a port,
/// and the flag of `DuplicateHandle` keeping the access of the handle duplicated.
const INVALID_HANDLE: i64 = -1;
const DUPLICATE_SAME_ACCESS: i64 = 2;

/// The thread-local slots a thread's environment block holds, and their count.
const TLS_SLOTS: u64 = 0x1480;
const TLS_MINIMUM: i64 = 64;

/// The handle numbers `GetStdHandle` takes for standard output and standard error.
const STD_OUTPUT: i64 = -11;
const STD_ERROR: i64 = -12;

/// The entries a thread takes from the completion port at once, the size of an
/// `OVERLAPPED_ENTRY` and the offsets of its completion key and `OVERLAPPED` address.
const ENTRIES: u64 = 16;
const ENTRY_SIZE: u64 = 32;
const ENTRY_KEY: u64 = 0;
const ENTRY_OVERLAPPED: u64 = 8;

/// The completion keys of a packet waking an executor thread, of the first termination
/// request and of a request's completion, and the timeout of a wait that never ends.
const KEY_WAKE: i64 = 1;
const KEY_TERMINATION: i64 = 2;
const KEY_REQUEST: i64 = 3;
const INFINITE: i64 = 0xFFFF_FFFF;

/// The status a termination request ends the process with, `STATUS_CONTROL_C_EXIT`.
const CONTROL_C_EXIT: i64 = 0xC000_013A;

/// The size of each thread's stack for stack overflow reports.
const TRAP_STACK: u64 = 65_536;

/// A task stack's guard region: a page no access reaches, the stack's floor, then the pages the
/// system guards. A touch of a guarded page while the stack is the running thread's makes the
/// system raise a stack overflow, those pages then holding the handler's frames.
pub const GUARD: u64 = 12_288;
const FLOOR: u64 = 4_096;
const PAGE_GUARD: i64 = 0x100;

/// The offsets in a thread's environment block of the bounds of the stack it runs on: its top,
/// the lowest address of its usable part, the base of its reservation, and the bytes of its
/// guard region the system keeps for a stack overflow's handler.
const STACK_BASE: u64 = 0x8;
const STACK_LIMIT: u64 = 0x10;
const DEALLOCATION_STACK: u64 = 0x1478;
const GUARANTEED_STACK_BYTES: u64 = 0x1748;

/// The code of a stack overflow exception, the offsets in an exception record of its parameter
/// count and of the parameter holding the faulting address, and the offsets of the record and
/// of the thread's saved registers in the exception pointers a vectored handler receives.
const STACK_OVERFLOW: i64 = 0xC000_00FD;
const PARAMETER_COUNT: u64 = 24;
const FAULT_ADDRESS: u64 = 40;
const POINTERS_RECORD: u64 = 0;
const POINTERS_CONTEXT: u64 = 8;

/// What a vectored exception handler returns to resume the thread with its saved registers, and
/// to pass the exception on.
const CONTINUE_EXECUTION: i64 = -1;
const CONTINUE_SEARCH: i64 = 0;

/// The 100-nanosecond intervals of processor time after which a seeded run ends, and the
/// milliseconds between two reads of the process's times.
const CPU_INTERVALS: i64 = 100_000_000;
const CPU_PERIOD: i64 = 100;

/// The layout of a thread's saved registers, a `CONTEXT`: its size, the offset of its flags and
/// the flags asking for its control and integer registers, and the offsets of the registers a
/// stop or fault handler reads and redirects.
struct Saved {
    size: u64,
    flags_at: u64,
    flags: i64,
    registers: Context,
}

fn saved(arch: Arch) -> Saved {
    match arch {
        Arch::Amd64 => Saved {
            size: 0x4D0,
            flags_at: 0x30,
            flags: 0x10_0003,
            registers: Context {
                arg: 0xB0,
                fp: 0xA0,
                sp: 0x98,
                ip: 0xF8,
                link: None,
            },
        },
        Arch::Aarch64 => Saved {
            size: 0x390,
            flags_at: 0,
            flags: 0x40_0003,
            registers: Context {
                arg: 0x8,
                fp: 0xF0,
                sp: 0x100,
                ip: 0x108,
                link: Some(0xF8),
            },
        },
    }
}

/// The calling convention of the system's functions on an architecture.
pub fn convention(arch: Arch) -> CallConv {
    match arch {
        Arch::Amd64 => CallConv::WindowsFastcall,
        Arch::Aarch64 => CallConv::SystemV,
    }
}

impl Lowering<'_, '_> {
    /// Calls a function of a system library, by its name for a `kernel32.dll` function and
    /// otherwise by its library's name, `!` and its name, through its word of the image's import
    /// address table, under the system's calling convention, giving its results.
    pub fn call_system(
        &mut self,
        fx: &mut Fx,
        name: &str,
        args: &[Value],
        returns: &[Type],
    ) -> Vec<Value> {
        let mut signature = Signature::new(convention(self.arch()));
        signature.params.extend(
            args.iter()
                .map(|arg| AbiParam::new(fx.b.func.dfg.value_type(*arg))),
        );
        signature
            .returns
            .extend(returns.iter().map(|ty| AbiParam::new(*ty)));
        let call = self.call_imported(fx, name, signature, args);
        fx.b.inst_results(call).to_vec()
    }

    /// Calls a function the image imports, named as `call_system` names it, through its word of
    /// the import address table, under `signature`.
    pub fn call_imported(
        &mut self,
        fx: &mut Fx,
        name: &str,
        signature: Signature,
        args: &[Value],
    ) -> Inst {
        let id = self
            .module
            .declare_data(
                &format!("{IMPORT_PREFIX}{name}"),
                Linkage::Import,
                false,
                false,
            )
            .unwrap_or_else(|error| panic!("the system function {name}: {error}"));
        let word = self.data_address(fx, id);
        let function = fx.load(PTR, word, 0);
        let signature = fx.b.import_signature(signature);
        let call = fx.b.ins().call_indirect(signature, function, args);
        system_site(fx, call);
        call
    }

    /// Calls the function at an address under the system's calling convention with
    /// pointer-sized arguments, giving its result of type `result`.
    pub fn call_address(
        &mut self,
        fx: &mut Fx,
        function: Value,
        args: &[Value],
        result: Type,
    ) -> Value {
        let signature = callback(self, args.len(), result);
        let signature = fx.b.import_signature(signature);
        let call = fx.b.ins().call_indirect(signature, function, args);
        system_site(fx, call);
        fx.b.inst_results(call)[0]
    }

    fn arch(&self) -> Arch {
        self.ck
            .db
            .target
            .arch()
            .expect("a Windows target's architecture")
    }
}

/// Records a call of the system's code as a stub's, which uses at most the stub call bound of
/// its caller's stack.
fn system_site(fx: &mut Fx, call: Inst) {
    let site = Site {
        kind: Kind::Stub,
        at: None,
    };
    fx.sites.push((call, site));
}

/// The signature, under the system's calling convention, of a function of `params`
/// pointer-sized arguments giving a result of type `result`.
fn callback(lo: &Lowering, params: usize, result: Type) -> Signature {
    let mut signature = Signature::new(convention(lo.arch()));
    signature
        .params
        .extend((0..params).map(|_| AbiParam::new(PTR)));
    signature.returns.push(AbiParam::new(result));
    signature
}

/// Declares and defines a function the system calls with `params` pointer-sized arguments,
/// giving a 32-bit result.
fn define_callback(
    lo: &mut Lowering,
    name: &str,
    params: usize,
    build: impl FnOnce(&mut Lowering, &mut Fx, &[Value]),
) -> FuncId {
    let signature = callback(lo, params, types::I32);
    let id = lo
        .module
        .declare_function(name, Linkage::Local, &signature)
        .expect("a function the system calls");
    lo.define(id, signature, lines::HIDDEN | lines::CRITICAL, |lo, fx| {
        let params = fx.entry();
        build(lo, fx, &params);
    });
    id
}

/// The address of a function the system calls.
fn function_address(lo: &mut Lowering, fx: &mut Fx, id: FuncId) -> Value {
    let reference = lo.func_ref(fx, id);
    fx.b.ins().func_addr(PTR, reference)
}

/// Defines the program entry seam, which the system enters the image at, and the glue running
/// the entry function as the first task.
pub fn entry_seam(lo: &mut Lowering, program: &Program) {
    let entry = super::entry_task(lo, program);
    let signature = Signature::new(convention(lo.arch()));
    let id = lo
        .module
        .declare_function(ENTRY, Linkage::Export, &signature)
        .expect("the program entry seam");
    lo.define(id, signature, lines::HIDDEN | lines::CRITICAL, |lo, fx| {
        fx.entry();
        lo.check_level(fx);
        system::start(lo, fx);
        super::run_entry(lo, fx, program, entry, &[]);
    });
}

/// Defines the entry seam of a test image, whose command line's words after the program's name
/// are the texts naming its run.
pub fn test_seam(lo: &mut Lowering, entry: FuncId) {
    let signature = Signature::new(convention(lo.arch()));
    let id = lo
        .module
        .declare_function(ENTRY, Linkage::Export, &signature)
        .expect("the test entry seam");
    lo.define(id, signature, lines::HIDDEN | lines::CRITICAL, |lo, fx| {
        fx.entry();
        system::start(lo, fx);
        let line = lo.call_system(fx, "GetCommandLineA", &[], &[PTR])[0];
        let (count, texts) = words(lo, fx, line);
        test_image::start_run(lo, fx, entry, count, texts);
    });
}

/// The words of a command line after the program's name, which a word in double quotes may
/// hold blanks in: their count and the address of their addresses, each word a NUL-terminated
/// text in a copy of the line.
fn words(lo: &mut Lowering, fx: &mut Fx, line: Value) -> (Value, Value) {
    let len = test_image::text_len(fx, line);
    let size = fx.b.ins().iadd_imm_s(len, 1);
    let copy = lo.record_alloc(fx, size);
    let slots = fx.b.ins().imul_imm_s(size, 8);
    let texts = lo.record_alloc(fx, slots);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, copy, 0);
    let none = fx.b.ins().icmp_imm_s(IntCC::Equal, texts, 0);
    let missing = fx.b.ins().bor(missing, none);
    fx.when(missing, |fx| system::unavailable(lo, fx));
    lo.call_routine(fx, Routine::Copy, &[copy, line, size]);
    let first = fx.load(types::I8, copy, 0);
    let quoted = fx.b.ins().icmp_imm_s(IntCC::Equal, first, i64::from(b'"'));
    let start = fx.b.ins().uextend(PTR, quoted);
    let name_end = fx.repeat(&[start], |fx, state| {
        let at = fx.b.ins().iadd(copy, state[0]);
        let byte = fx.load(types::I8, at, 0);
        let ended = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, 0);
        let quote = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'"'));
        let blank = is_blank(fx, byte);
        let closing = fx.b.ins().select(quoted, quote, blank);
        let stop = fx.b.ins().bor(ended, closing);
        let next = fx.b.ins().iadd_imm_s(state[0], 1);
        let index = fx.b.ins().select(stop, state[0], next);
        (fx.b.ins().bxor_imm_s(stop, 1), vec![index])
    })[0];
    let zero = fx.ptr(0);
    let result = fx.repeat(&[name_end, zero, zero], |fx, state| {
        let (index, count, inside) = (state[0], state[1], state[2]);
        let at = fx.b.ins().iadd(copy, index);
        let byte = fx.load(types::I8, at, 0);
        let ended = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, 0);
        let separator = is_blank(fx, byte);
        let quote = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'"'));
        let separator = fx.b.ins().bor(separator, quote);
        let outside = fx.b.ins().icmp_imm_s(IntCC::Equal, inside, 0);
        let starts = fx.b.ins().bxor_imm_s(separator, 1);
        let starts = fx.b.ins().band(starts, outside);
        let ending = fx.b.ins().bxor_imm_s(ended, 1);
        let starts = fx.b.ins().band(starts, ending);
        fx.when(starts, |fx| {
            let slot = fx.b.ins().imul_imm_s(count, 8);
            let slot = fx.b.ins().iadd(texts, slot);
            fx.store(at, slot, 0);
        });
        fx.when(separator, |fx| {
            let nul = fx.int(types::I8, 0);
            fx.store(nul, at, 0);
        });
        let counted = fx.b.ins().iadd_imm_s(count, 1);
        let count = fx.b.ins().select(starts, counted, count);
        let one = fx.ptr(1);
        let zero = fx.ptr(0);
        let inside = fx.b.ins().select(separator, zero, inside);
        let inside = fx.b.ins().select(starts, one, inside);
        let next = fx.b.ins().iadd_imm_s(index, 1);
        (ending, vec![next, count, inside])
    });
    (result[1], texts)
}

/// Whether a byte is a space or a tab.
fn is_blank(fx: &mut Fx, byte: Value) -> Value {
    let space = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b' '));
    let tab = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'\t'));
    fx.b.ins().bor(space, tab)
}

/// Writes a byte range to standard output, for `fd` 1, or standard error, for 2, whole unless
/// the stream refuses.
pub fn write_all(lo: &mut Lowering, fx: &mut Fx, fd: i64, text: Value, len: Value) {
    let which = fx.int(types::I32, if fd == 1 { STD_OUTPUT } else { STD_ERROR });
    let handle = lo.call_system(fx, "GetStdHandle", &[which], &[PTR])[0];
    let written = fx.slot(8, 8);
    let none = fx.ptr(0);
    fx.repeat(&[text, len], |fx, state| {
        let (at, left) = (state[0], state[1]);
        let most = fx.ptr(u64::from(u32::MAX));
        let count = fx.b.ins().umin(left, most);
        let count = fx.b.ins().ireduce(types::I32, count);
        let zero = fx.int(types::I32, 0);
        fx.store(zero, written, 0);
        let ok = lo.call_system(
            fx,
            "WriteFile",
            &[handle, at, count, written, none],
            &[types::I32],
        )[0];
        let done = fx.load(types::I32, written, 0);
        let done = fx.b.ins().uextend(PTR, done);
        let wrote = fx.b.ins().icmp_imm_s(IntCC::NotEqual, ok, 0);
        let progressed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, done, 0);
        let progressed = fx.b.ins().band(wrote, progressed);
        let next_at = fx.b.ins().iadd(at, done);
        let next_left = fx.b.ins().isub(left, done);
        let zero = fx.ptr(0);
        let next_left = fx.b.ins().select(progressed, next_left, zero);
        let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next_left, 0);
        (more, vec![next_at, next_left])
    });
}

/// Defines the record routines, over the process heap, a resize of no record allocating one,
/// and `Exit`.
pub fn define(lo: &mut Lowering, fx: &mut Fx, routine: Routine) {
    let params = fx.entry();
    match routine {
        Routine::RecordAlloc => {
            let heap = lo.call_system(fx, "GetProcessHeap", &[], &[PTR])[0];
            let zeroed = fx.int(types::I32, HEAP_ZERO_MEMORY);
            let address = lo.call_system(fx, "HeapAlloc", &[heap, zeroed, params[0]], &[PTR])[0];
            fx.b.ins().return_(&[address]);
        }
        Routine::RecordFree => {
            let heap = lo.call_system(fx, "GetProcessHeap", &[], &[PTR])[0];
            let flags = fx.int(types::I32, 0);
            lo.call_system(fx, "HeapFree", &[heap, flags, params[0]], &[types::I32]);
            fx.b.ins().return_(&[]);
        }
        Routine::RecordResize => {
            let heap = lo.call_system(fx, "GetProcessHeap", &[], &[PTR])[0];
            let flags = fx.int(types::I32, 0);
            let held = fx.b.ins().icmp_imm_s(IntCC::NotEqual, params[0], 0);
            let address = fx.choose_in(
                lo,
                held,
                &[PTR],
                |lo, fx| {
                    let args = [heap, flags, params[0], params[1]];
                    lo.call_system(fx, "HeapReAlloc", &args, &[PTR])
                },
                |lo, fx| lo.call_system(fx, "HeapAlloc", &[heap, flags, params[1]], &[PTR]),
            )[0];
            fx.b.ins().return_(&[address]);
        }
        Routine::Exit => {
            let status = fx.b.ins().ireduce(types::I32, params[0]);
            exit(lo, fx, status);
        }
        _ => unreachable!("a Windows routine"),
    }
}

/// Ends the process with a 32-bit status.
fn exit(lo: &mut Lowering, fx: &mut Fx, status: Value) {
    lo.call_system(fx, "ExitProcess", &[status], &[]);
    fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
}

/// The record of the executor thread running the caller, zero outside one: the thread-local
/// slot `Init` allocated, read from the running thread's environment block.
pub fn current_thread(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let teb = lo.call_stub(fx, Stub::Teb, &[])[0];
    let state_address = lo.state(fx);
    let index = fx.load(PTR, state_address, state::TLS_INDEX);
    let offset = fx.b.ins().imul_imm_s(index, 8);
    let slot = fx.b.ins().iadd(teb, offset);
    fx.load(PTR, slot, TLS_SLOTS)
}

/// Makes a record the running thread's, keeping a handle to the thread in it, and gives the
/// thread a stack for stack overflow reports; nonzero when the system refuses one.
pub fn thread_start(lo: &mut Lowering, fx: &mut Fx, record: Value) -> Value {
    let state_address = lo.state(fx);
    let index = fx.load(types::I32, state_address, state::TLS_INDEX);
    lo.call_system(fx, "TlsSetValue", &[index, record], &[types::I32]);
    let process = lo.call_system(fx, "GetCurrentProcess", &[], &[PTR])[0];
    let current = lo.call_system(fx, "GetCurrentThread", &[], &[PTR])[0];
    let handle = fx.slot(8, 8);
    let access = fx.int(types::I32, 0);
    let inherit = fx.int(types::I32, 0);
    let same = fx.int(types::I32, DUPLICATE_SAME_ACCESS);
    let duplicated = lo.call_system(
        fx,
        "DuplicateHandle",
        &[process, current, process, handle, access, inherit, same],
        &[types::I32],
    )[0];
    let handle = fx.load(PTR, handle, 0);
    fx.store(handle, record, thread::SYSTEM);
    let size = fx.ptr(TRAP_STACK);
    let stack = commit(lo, fx, size);
    let no_handle = fx.b.ins().icmp_imm_s(IntCC::Equal, duplicated, 0);
    let no_stack = fx.b.ins().icmp_imm_s(IntCC::Equal, stack, 0);
    let top = fx.b.ins().iadd_imm_s(stack, TRAP_STACK as i64);
    fx.store(top, record, thread::TRAP_STACK);
    fx.b.ins().bor(no_handle, no_stack)
}

/// Reserves and commits `len` bytes that can be read and written, zero-filled and aligned to
/// the system's allocation granularity; zero when the system refuses.
fn commit(lo: &mut Lowering, fx: &mut Fx, len: Value) -> Value {
    let anywhere = fx.ptr(0);
    let kind = fx.int(types::I32, MEM_COMMIT_RESERVE);
    let protection = fx.int(types::I32, PAGE_READWRITE);
    lo.call_system(
        fx,
        "VirtualAlloc",
        &[anywhere, len, kind, protection],
        &[PTR],
    )[0]
}

/// The executor threads of `Init`: one per processor of the process's affinity set, the calling
/// thread being the first, which all wait on the process's completion port, after the handlers
/// of stack overflows and termination requests; returns 1 from `Init` when the system refuses
/// one.
pub fn start_threads(lo: &mut Lowering, fx: &mut Fx) {
    let state_address = lo.state(fx);
    allocate_slot(lo, fx);
    if lo.seeded() {
        start_seeded(lo, fx);
        return;
    }
    let invalid = fx.int(PTR, INVALID_HANDLE);
    let zero = fx.ptr(0);
    let concurrency = fx.int(types::I32, 0);
    let port = lo.call_system(
        fx,
        "CreateIoCompletionPort",
        &[invalid, zero, zero, concurrency],
        &[PTR],
    )[0];
    unavailable_unless(fx, port);
    fx.store(port, state_address, state::PORT);
    let count = processors(lo, fx);
    let threads = thread_records(lo, fx, count);
    let first = fx.load(PTR, threads, 0);
    let refused = thread_start(lo, fx, first);
    unavailable_if(fx, refused);
    handle_faults(lo, fx);
    let handler = control_handler(lo);
    let handler = function_address(lo, fx, handler);
    let add = fx.int(types::I32, 1);
    let installed = lo.call_system(fx, "SetConsoleCtrlHandler", &[handler, add], &[types::I32])[0];
    let refused = fx.b.ins().icmp_imm_s(IntCC::Equal, installed, 0);
    unavailable_if(fx, refused);
    let start = thread_glue(lo);
    let start = function_address(lo, fx, start);
    let one = fx.ptr(1);
    fx.each(one, count, |fx, index| {
        let slot = fx.b.ins().imul_imm_s(index, 8);
        let slot = fx.b.ins().iadd(threads, slot);
        let record = fx.load(PTR, slot, 0);
        let created = spawn_thread(lo, fx, start, record);
        unavailable_unless(fx, created);
        lo.call_system(fx, "CloseHandle", &[created], &[types::I32]);
    });
}

/// Allocates the thread-local slot holding each executor thread's record, which the thread's
/// environment block holds itself.
fn allocate_slot(lo: &mut Lowering, fx: &mut Fx) {
    let index = lo.call_system(fx, "TlsAlloc", &[], &[types::I32])[0];
    let index = fx.b.ins().uextend(PTR, index);
    let beyond =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, index, TLS_MINIMUM);
    unavailable_if(fx, beyond);
    let state_address = lo.state(fx);
    fx.store(index, state_address, state::TLS_INDEX);
}

/// The records of `count` executor threads, numbered from 0, in a new array of their addresses
/// the runtime state keeps.
fn thread_records(lo: &mut Lowering, fx: &mut Fx, count: Value) -> Value {
    let state_address = lo.state(fx);
    fx.store(count, state_address, state::THREAD_COUNT);
    let size = fx.b.ins().imul_imm_s(count, 8);
    let threads = lo.record_alloc(fx, size);
    unavailable_unless(fx, threads);
    fx.store(threads, state_address, state::THREADS);
    let zero = fx.ptr(0);
    fx.each(zero, count, |fx, index| {
        let size = fx.ptr(thread::SIZE);
        let record = lo.record_alloc(fx, size);
        unavailable_unless(fx, record);
        fx.store(index, record, thread::INDEX);
        let slot = fx.b.ins().imul_imm_s(index, 8);
        let slot = fx.b.ins().iadd(threads, slot);
        fx.store(record, slot, 0);
    });
    threads
}

/// A new thread of the process running `start` with an argument, zero when the system refuses.
fn spawn_thread(lo: &mut Lowering, fx: &mut Fx, start: Value, argument: Value) -> Value {
    let none = fx.ptr(0);
    let flags = fx.int(types::I32, 0);
    lo.call_system(
        fx,
        "CreateThread",
        &[none, none, start, argument, flags, none],
        &[PTR],
    )[0]
}

/// The executor of `Init` for the seeded scheduler: the calling thread is the one executor
/// thread, without the completion port; a stack overflow is reported, and the run ends with
/// status `TIMED_OUT` once the process has used 10 seconds of processor time.
fn start_seeded(lo: &mut Lowering, fx: &mut Fx) {
    let one = fx.ptr(1);
    let threads = thread_records(lo, fx, one);
    let record = fx.load(PTR, threads, 0);
    let refused = thread_start(lo, fx, record);
    unavailable_if(fx, refused);
    handle_faults(lo, fx);
    let watchdog = cpu_watchdog(lo);
    let watchdog = function_address(lo, fx, watchdog);
    let zero = fx.ptr(0);
    let created = spawn_thread(lo, fx, watchdog, zero);
    unavailable_unless(fx, created);
    lo.call_system(fx, "CloseHandle", &[created], &[types::I32]);
}

/// The thread ending a seeded run with status `TIMED_OUT` once the process has used 10 seconds
/// of processor time, which it reads every 100 milliseconds.
fn cpu_watchdog(lo: &mut Lowering) -> FuncId {
    define_callback(lo, "_NSreplay_cpu_limit", 1, |lo, fx, _| {
        let times = fx.slot(32, 8);
        let period = fx.int(types::I32, CPU_PERIOD);
        let process = lo.call_system(fx, "GetCurrentProcess", &[], &[PTR])[0];
        fx.repeat(&[], |fx, _| {
            lo.call_system(fx, "Sleep", &[period], &[]);
            let creation = times;
            let exited = fx.b.ins().iadd_imm_s(times, 8);
            let kernel = fx.b.ins().iadd_imm_s(times, 16);
            let user = fx.b.ins().iadd_imm_s(times, 24);
            lo.call_system(
                fx,
                "GetProcessTimes",
                &[process, creation, exited, kernel, user],
                &[types::I32],
            );
            let kernel = fx.load(PTR, times, 16);
            let user = fx.load(PTR, times, 24);
            let used = fx.b.ins().iadd(kernel, user);
            let over =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, used, CPU_INTERVALS);
            fx.when(over, |fx| {
                let status = fx.int(types::I32, i64::from(test_image::TIMED_OUT));
                exit(lo, fx, status);
            });
            (fx.int(types::I8, 1), Vec::new())
        });
        fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
    })
}

/// The function each executor thread but the first starts in, which the system calls with the
/// thread's record: it runs `ThreadMain`.
fn thread_glue(lo: &mut Lowering) -> FuncId {
    define_callback(lo, "_NSstack_switch_thread_start", 1, |lo, fx, params| {
        lo.call_routine(fx, Routine::ThreadMain, &[params[0]]);
        fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
    })
}

/// The number of processors in the process's affinity set, at least 1.
fn processors(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let masks = fx.slot(16, 8);
    let zero = fx.ptr(0);
    fx.store(zero, masks, 0);
    let process = lo.call_system(fx, "GetCurrentProcess", &[], &[PTR])[0];
    let system = fx.b.ins().iadd_imm_s(masks, 8);
    lo.call_system(
        fx,
        "GetProcessAffinityMask",
        &[process, masks, system],
        &[types::I32],
    );
    let mask = fx.load(PTR, masks, 0);
    let count = fx.b.ins().popcnt(mask);
    let one = fx.ptr(1);
    fx.b.ins().umax(count, one)
}

/// Installs the vectored exception handler receiving stack overflows; returns 1 from `Init`
/// when the system refuses it.
fn handle_faults(lo: &mut Lowering, fx: &mut Fx) {
    let handler = fault_handler(lo);
    let handler = function_address(lo, fx, handler);
    let first = fx.ptr(1);
    let added = lo.call_system(fx, "AddVectoredExceptionHandler", &[first, handler], &[PTR])[0];
    unavailable_unless(fx, added);
}

/// The vectored exception handler: a stack overflow in the guard region of the running task's
/// stack resumes in `OverflowRaise` on the thread's trap stack, which becomes the stack the
/// thread's environment block records; every other exception passes on.
fn fault_handler(lo: &mut Lowering) -> FuncId {
    define_callback(lo, "_NStrap_overflow", 1, |lo, fx, params| {
        let record = fx.load(PTR, params[0], POINTERS_RECORD);
        let context = fx.load(PTR, params[0], POINTERS_CONTEXT);
        let code = fx.load(types::I32, record, 0);
        let code = fx.b.ins().uextend(PTR, code);
        let overflow = fx.b.ins().icmp_imm_s(IntCC::Equal, code, STACK_OVERFLOW);
        fx.when(overflow, |fx| {
            let registers = saved(lo.arch()).registers;
            let count = fx.load(types::I32, record, PARAMETER_COUNT);
            let addressed =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, count, 2);
            let fault = fx.choose(
                addressed,
                &[PTR],
                |fx| vec![fx.load(PTR, record, FAULT_ADDRESS)],
                |fx| vec![fx.load(PTR, context, registers.sp)],
            )[0];
            let thread = lo.thread(fx);
            let guarded = super::overflow_interrupted(lo, fx, thread, fault, context, registers);
            fx.when(guarded, |fx| {
                let top = fx.load(PTR, thread, thread::TRAP_STACK);
                let low = fx.b.ins().iadd_imm_s(top, -(TRAP_STACK as i64));
                let none = fx.int(types::I32, 0);
                set_stack_bounds(lo, fx, [top, low, low, none]);
                let resume = fx.int(types::I32, CONTINUE_EXECUTION);
                fx.b.ins().return_(&[resume]);
            });
        });
        let search = fx.int(types::I32, CONTINUE_SEARCH);
        fx.b.ins().return_(&[search]);
    })
}

/// The console's control handler, which the system calls on a thread of its own with each
/// control event: the first is a termination request it records and passes to the executor
/// threads through the completion port; a later one ends the process at once.
fn control_handler(lo: &mut Lowering) -> FuncId {
    define_callback(lo, "_NSstack_switch_terminations", 1, |lo, fx, _| {
        let state_address = lo.state(fx);
        let word = fx.offset(state_address, state::TERMINATION);
        let zero = fx.ptr(0);
        let status = fx.int(PTR, CONTROL_C_EXIT);
        let before =
            fx.b.ins()
                .atomic_cas(MemFlagsData::trusted(), word, zero, status);
        let later = fx.b.ins().icmp_imm_s(IntCC::NotEqual, before, 0);
        fx.when(later, |fx| {
            let status = fx.int(types::I32, CONTROL_C_EXIT);
            exit(lo, fx, status);
        });
        let none = fx.ptr(0);
        post(lo, fx, KEY_TERMINATION, none);
        let handled = fx.int(types::I32, 1);
        fx.b.ins().return_(&[handled]);
    })
}

/// Stops the task an executor thread runs when it may stop where it runs: suspends the thread,
/// redirects its saved registers as a stop signal's handler does, resumes it and lets it run for
/// a millisecond before it may be suspended again. A task that has just started running may not
/// have recorded its thread yet: a record of none, or of the calling thread, is left alone.
pub fn stop_thread(lo: &mut Lowering, fx: &mut Fx, thread: Value) {
    let current = lo.thread(fx);
    let other = fx.b.ins().icmp(IntCC::NotEqual, thread, current);
    let recorded = fx.b.ins().icmp_imm_s(IntCC::NotEqual, thread, 0);
    let other = fx.b.ins().band(other, recorded);
    fx.when(other, |fx| suspend_and_stop(lo, fx, thread));
}

fn suspend_and_stop(lo: &mut Lowering, fx: &mut Fx, thread: Value) {
    let handle = fx.load(PTR, thread, thread::SYSTEM);
    let count = lo.call_system(fx, "SuspendThread", &[handle], &[types::I32])[0];
    let suspended = fx.b.ins().icmp_imm_s(IntCC::NotEqual, count, -1);
    fx.when(suspended, |fx| {
        let layout = saved(lo.arch());
        let context = fx.slot(layout.size, 16);
        let flags = fx.int(types::I32, layout.flags);
        fx.store(flags, context, layout.flags_at);
        let read = lo.call_system(fx, "GetThreadContext", &[handle, context], &[types::I32])[0];
        let read = fx.b.ins().icmp_imm_s(IntCC::NotEqual, read, 0);
        fx.when(read, |fx| {
            let before = fx.load(PTR, context, layout.registers.ip);
            super::stop_interrupted(lo, fx, thread, context, layout.registers);
            let after = fx.load(PTR, context, layout.registers.ip);
            let redirected = fx.b.ins().icmp(IntCC::NotEqual, before, after);
            fx.when(redirected, |fx| {
                lo.call_system(fx, "SetThreadContext", &[handle, context], &[types::I32]);
            });
        });
        lo.call_system(fx, "ResumeThread", &[handle], &[types::I32]);
        let period = fx.int(types::I32, 1);
        lo.call_system(fx, "Sleep", &[period], &[]);
    });
}

/// Waits idle on the completion port until a packet arrives, unless a task is ready, then
/// handles the packets taken.
pub fn idle(lo: &mut Lowering, fx: &mut Fx) {
    let current = lo.thread(fx);
    let idle = fx.offset(current, thread::IDLE);
    let one = fx.ptr(1);
    fx.atomic_store(one, idle);
    let state_address = lo.state(fx);
    let head = fx.offset(state_address, state::READY_HEAD);
    let head = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), head);
    let empty = fx.b.ins().icmp_imm_s(IntCC::Equal, head, 0);
    take_packets(lo, fx, Some(empty));
    let zero = fx.ptr(0);
    fx.atomic_store(zero, idle);
}

/// Takes the packets the completion port holds, when `wait` is given and nonzero waiting for
/// one until the first timer is due, and handles them, then delivers the expiries of the timers
/// due.
pub fn take_packets(lo: &mut Lowering, fx: &mut Fx, wait: Option<Value>) {
    let entries = fx.slot(ENTRIES * ENTRY_SIZE, 8);
    let removed = fx.slot(8, 8);
    let state_address = lo.state(fx);
    let port = fx.load(PTR, state_address, state::PORT);
    let count = fx.int(types::I32, ENTRIES as i64);
    let timeout = match wait {
        Some(wait) => fx.choose_in(
            lo,
            wait,
            &[types::I32],
            |lo, fx| vec![first_timeout(lo, fx)],
            |_, fx| vec![fx.int(types::I32, 0)],
        )[0],
        None => fx.int(types::I32, 0),
    };
    let alertable = fx.int(types::I32, 0);
    let taken = lo.call_system(
        fx,
        "GetQueuedCompletionStatusEx",
        &[port, entries, count, removed, timeout, alertable],
        &[types::I32],
    )[0];
    let taken = fx.b.ins().icmp_imm_s(IntCC::NotEqual, taken, 0);
    fx.when(taken, |fx| {
        let count = fx.load(types::I32, removed, 0);
        let count = fx.b.ins().uextend(PTR, count);
        let zero = fx.ptr(0);
        fx.each(zero, count, |fx, index| {
            let offset = fx.b.ins().imul_imm_s(index, ENTRY_SIZE as i64);
            let entry = fx.b.ins().iadd(entries, offset);
            let key = fx.load(PTR, entry, ENTRY_KEY);
            let overlapped = fx.load(PTR, entry, ENTRY_OVERLAPPED);
            handle_packet(lo, fx, key, overlapped);
        });
    });
    expire(lo, fx);
}

/// Handles a packet of the completion port: a request's completion is delivered to its
/// operation, and the first termination request cancels the entry task's construct; a wake needs
/// nothing more.
fn handle_packet(lo: &mut Lowering, fx: &mut Fx, key: Value, overlapped: Value) {
    let request = fx.b.ins().icmp_imm_s(IntCC::Equal, key, KEY_REQUEST);
    fx.when(request, |fx| {
        let op = fx.b.ins().iadd_imm_s(overlapped, requests::OP as i64);
        let result = fx.int(types::I32, 0);
        tasks::deliver(lo, fx, op, result);
    });
    let termination = fx.b.ins().icmp_imm_s(IntCC::Equal, key, KEY_TERMINATION);
    fx.when(termination, |fx| {
        let state_address = lo.state(fx);
        let root = fx.load(PTR, state_address, state::ROOT);
        lo.call_routine(fx, Routine::Cancel, &[root]);
    });
}

/// The milliseconds until the first timer is due, rounded up, `INFINITE` without one.
fn first_timeout(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::TIMER_LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let first = fx.load(PTR, state_address, state::TIMERS);
    let armed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, first, 0);
    let due = fx.choose(
        armed,
        &[PTR],
        |fx| vec![fx.load(PTR, first, operation::DUE)],
        |fx| vec![fx.int(PTR, -1)],
    )[0];
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    let now = lo.now_ns(fx);
    let ahead = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, due, now);
    let left = fx.b.ins().isub(due, now);
    let zero = fx.ptr(0);
    let left = fx.b.ins().select(ahead, left, zero);
    let per_ms = fx.ptr(timers::NS_PER_MS);
    let ms = fx.b.ins().udiv(left, per_ms);
    let rest = fx.b.ins().urem(left, per_ms);
    let partial = fx.b.ins().icmp_imm_s(IntCC::NotEqual, rest, 0);
    let partial = fx.b.ins().uextend(PTR, partial);
    let ms = fx.b.ins().iadd(ms, partial);
    let longest = fx.ptr((INFINITE - 1) as u64);
    let ms = fx.b.ins().umin(ms, longest);
    let ms = fx.b.ins().ireduce(types::I32, ms);
    let forever = fx.int(types::I32, INFINITE);
    fx.b.ins().select(armed, ms, forever)
}

/// `ClockNow`: the nanoseconds of the monotonic clock, the performance counter's.
pub fn define_clock_now(lo: &mut Lowering, fx: &mut Fx) {
    fx.entry();
    let words = fx.slot(16, 8);
    let frequency = fx.b.ins().iadd_imm_s(words, 8);
    lo.call_system(fx, "QueryPerformanceCounter", &[words], &[types::I32]);
    lo.call_system(fx, "QueryPerformanceFrequency", &[frequency], &[types::I32]);
    let count = fx.load(PTR, words, 0);
    let rate = fx.load(PTR, words, 8);
    let one = fx.ptr(1);
    let rate = fx.b.ins().umax(rate, one);
    let billion = fx.ptr(1_000_000_000);
    let seconds = fx.b.ins().udiv(count, rate);
    let rest = fx.b.ins().urem(count, rate);
    let whole = fx.b.ins().imul(seconds, billion);
    let part = fx.b.ins().imul(rest, billion);
    let part = fx.b.ins().udiv(part, rate);
    let now = fx.b.ins().iadd(whole, part);
    fx.b.ins().return_(&[now]);
}

/// Arms a timer of the monotonic clock due `ns` nanoseconds from now, through `TimerArm`.
pub fn arm(lo: &mut Lowering, fx: &mut Fx, op: Value, ns: Value) {
    lo.call_routine(fx, Routine::TimerArm, &[op, ns]);
}

/// The body of `TimerArm`: arms the timer in the timers the runtime keeps, and wakes a thread
/// waiting idle, so that it waits no longer than the timer.
pub fn arm_timer(lo: &mut Lowering, fx: &mut Fx, op: Value, ns: Value) {
    let state_address = lo.state(fx);
    let lock = fx.offset(state_address, state::TIMER_LOCK);
    lo.call_routine(fx, Routine::Lock, &[lock]);
    let now = lo.now_ns(fx);
    let due = timers::due_after(fx, now, ns);
    timers::arm(lo, fx, op, due);
    lo.call_routine(fx, Routine::Unlock, &[lock]);
    let none = fx.ptr(0);
    post(lo, fx, KEY_WAKE, none);
}

/// Takes each timer due out of the timers, one at a time under the timer lock, and delivers its
/// expiry outside it.
fn expire(lo: &mut Lowering, fx: &mut Fx) {
    let now = lo.now_ns(fx);
    fx.repeat(&[], |fx, _| {
        let state_address = lo.state(fx);
        let lock = fx.offset(state_address, state::TIMER_LOCK);
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let first = fx.load(PTR, state_address, state::TIMERS);
        let armed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, first, 0);
        let due = fx.choose(
            armed,
            &[types::I8],
            |fx| {
                let due = fx.load(PTR, first, operation::DUE);
                vec![fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, due, now)]
            },
            |fx| vec![fx.int(types::I8, 0)],
        )[0];
        fx.when(due, |fx| {
            timers::disarm(lo, fx, first);
            timers::claim(fx, first);
        });
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        fx.when(due, |fx| {
            let expired = fx.int(types::I32, tasks::EXPIRED);
            tasks::deliver(lo, fx, first, expired);
        });
        (due, Vec::new())
    });
}

/// Asks once for the cancellation of an operation whose record stays valid during the call: a
/// timer armed and not expired leaves the timers and completes cancelled at once, and a request
/// in flight is cancelled as `cancel_request` does.
pub fn cancel_operation(lo: &mut Lowering, fx: &mut Fx, op: Value) {
    let claimed = timers::claim_armed(lo, fx, op);
    fx.when(claimed, |fx| tasks::deliver_cancelled(lo, fx, op));
    let kind = fx.load(PTR, op, operation::KIND);
    let wait = fx.b.ins().icmp_imm_s(IntCC::Equal, kind, tasks::kind::WAIT);
    let handle = fx.load(PTR, op, operation::WAITER);
    let request = fx.b.ins().icmp_imm_s(IntCC::NotEqual, handle, 0);
    let request = fx.b.ins().band(wait, request);
    let done = fx.load(PTR, op, operation::DONE);
    let pending = fx.b.ins().icmp_imm_s(IntCC::Equal, done, 0);
    let request = fx.b.ins().band(request, pending);
    fx.when(request, |fx| cancel_request(lo, fx, op));
}

/// Wakes an executor thread waiting idle, other than the running one, so a task made ready
/// runs: a wake packet on the completion port, posted when another thread is idle, which
/// whichever thread waits there takes. Each thread clears its own idle flag once its wait ends,
/// so a packet another thread took leaves a thread still waiting marked idle.
pub fn wake_idle(lo: &mut Lowering, fx: &mut Fx) {
    let state_address = lo.state(fx);
    let threads = fx.load(PTR, state_address, state::THREADS);
    let count = fx.load(PTR, state_address, state::THREAD_COUNT);
    let current = lo.thread(fx);
    let zero = fx.ptr(0);
    fx.repeat(&[zero], |fx, state| {
        let index = state[0];
        let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, count);
        let next = fx.choose_in(
            lo,
            inside,
            &[PTR, types::I8],
            |lo, fx| {
                let slot = fx.b.ins().imul_imm_s(index, 8);
                let slot = fx.b.ins().iadd(threads, slot);
                let record = fx.load(PTR, slot, 0);
                let other = fx.b.ins().icmp(IntCC::NotEqual, record, current);
                let idle = fx.offset(record, thread::IDLE);
                let woken = fx.choose(
                    other,
                    &[types::I8],
                    |fx| {
                        let zero = fx.ptr(0);
                        let seen = fetch_add(fx, idle, zero);
                        vec![fx.b.ins().icmp_imm_s(IntCC::Equal, seen, 1)]
                    },
                    |fx| vec![fx.int(types::I8, 0)],
                )[0];
                fx.when(woken, |fx| {
                    let none = fx.ptr(0);
                    post(lo, fx, KEY_WAKE, none);
                });
                let next = fx.b.ins().iadd_imm_s(index, 1);
                vec![next, fx.b.ins().bxor_imm_s(woken, 1)]
            },
            |_, fx| vec![index, fx.int(types::I8, 0)],
        );
        (next[1], vec![next[0]])
    });
}

/// Posts a packet of a completion key and an `OVERLAPPED` address to the completion port.
fn post(lo: &mut Lowering, fx: &mut Fx, key: i64, overlapped: Value) {
    let state_address = lo.state(fx);
    let port = fx.load(PTR, state_address, state::PORT);
    let count = fx.int(types::I32, 0);
    let key = fx.int(PTR, key);
    lo.call_system(
        fx,
        "PostQueuedCompletionStatus",
        &[port, count, key, overlapped],
        &[types::I32],
    );
}

/// Maps `count` chunks, which the allocation granularity aligns to their size; zero when the
/// system refuses.
pub fn map_chunks(lo: &mut Lowering, fx: &mut Fx, count: Value) -> Value {
    let len =
        fx.b.ins()
            .imul_imm_s(count, super::super::runtime::memory::CHUNK as i64);
    commit(lo, fx, len)
}

/// Releases memory `commit` or `map_chunks` gave, a whole reservation at once.
pub fn unmap(lo: &mut Lowering, fx: &mut Fx, address: Value) {
    let whole = fx.ptr(0);
    let release = fx.int(types::I32, MEM_RELEASE);
    lo.call_system(fx, "VirtualFree", &[address, whole, release], &[types::I32]);
}

/// Maps a stack of `len` bytes, its guard region protected when `guarded`, zero when the system
/// refuses.
pub fn map_stack(lo: &mut Lowering, fx: &mut Fx, len: Value, guarded: bool) -> Value {
    let base = commit(lo, fx, len);
    if guarded {
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, base, 0);
        fx.when(present, |fx| protect_guard(lo, fx, base, false));
    }
    base
}

/// Protects the guard region at the base of a stack, its floor inaccessible and the rest
/// guarded, or makes it accessible again.
pub fn protect_guard(lo: &mut Lowering, fx: &mut Fx, base: Value, accessible: bool) {
    let parts = match accessible {
        true => vec![(0, GUARD, PAGE_READWRITE)],
        false => vec![
            (0, FLOOR, PAGE_NOACCESS),
            (FLOOR, GUARD - FLOOR, PAGE_READWRITE | PAGE_GUARD),
        ],
    };
    let old = fx.slot(8, 8);
    for (offset, len, protection) in parts {
        let at = fx.b.ins().iadd_imm_s(base, offset as i64);
        let len = fx.ptr(len);
        let protection = fx.int(types::I32, protection);
        lo.call_system(
            fx,
            "VirtualProtect",
            &[at, len, protection, old],
            &[types::I32],
        );
    }
}

/// The bounds of the stack the running thread runs on, as its environment block records them.
pub fn stack_bounds(lo: &mut Lowering, fx: &mut Fx) -> [Value; 4] {
    let teb = lo.call_stub(fx, Stub::Teb, &[])[0];
    [
        fx.load(PTR, teb, STACK_BASE),
        fx.load(PTR, teb, STACK_LIMIT),
        fx.load(PTR, teb, DEALLOCATION_STACK),
        fx.load(types::I32, teb, GUARANTEED_STACK_BYTES),
    ]
}

/// Records in the running thread's environment block the bounds of the stack it runs on, as
/// `stack_bounds` gives them.
pub fn set_stack_bounds(lo: &mut Lowering, fx: &mut Fx, bounds: [Value; 4]) {
    let teb = lo.call_stub(fx, Stub::Teb, &[])[0];
    let [base, limit, deallocation, guaranteed] = bounds;
    fx.store(base, teb, STACK_BASE);
    fx.store(limit, teb, STACK_LIMIT);
    fx.store(deallocation, teb, DEALLOCATION_STACK);
    fx.store(guaranteed, teb, GUARANTEED_STACK_BYTES);
}

/// Records a task's own stack as the running thread's: from its base, its guard region, then
/// its usable part up to its top.
pub fn enter_task_stack(lo: &mut Lowering, fx: &mut Fx, record: Value) {
    let base = fx.load(PTR, record, task::STACK);
    let size = fx.load(PTR, record, task::STACK_SIZE);
    let low = fx.load(PTR, record, task::GUARD);
    let top = fx.b.ins().iadd(base, size);
    let guarded = fx.int(types::I32, (GUARD - FLOOR) as i64);
    set_stack_bounds(lo, fx, [top, low, base, guarded]);
}

/// Records a segment from `low` to `high`, which has no guard region, as the running thread's
/// stack.
pub fn enter_segment(lo: &mut Lowering, fx: &mut Fx, low: Value, high: Value) {
    let none = fx.int(types::I32, 0);
    set_stack_bounds(lo, fx, [high, low, low, none]);
}
