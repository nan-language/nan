//! System capabilities: the root grant, the operations of `sys::Fs`, `sys::Net`, `sys::Clock`,
//! `sys::Random`, `sys::Log` and `sys::Stdio` through the host platform, or in a test image
//! through the sims, files, listeners and connections, revocation, and the submission of
//! io_uring operations.
//!
//! Each operation checks its capability's entry first. An `Fs` entry's state is its directory's
//! descriptor, the address and length of its path prefix, and whether it is read-only; a `Net`
//! entry's is the address and length of the one host it reaches, when restricted. A file,
//! listener or connection is the entry it was opened through and the holding of its
//! descriptor; a revoker is the holding of its entry or cell.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::body::{Body, Held};
use super::super::call::Called;
use super::super::fx::{Fx, PTR};
use super::super::runtime::tables::{entry, kind, live_entry};
use super::super::runtime::{Routine, holdings};
use super::super::transfer;
use super::Supply;
use super::render;
use super::sims;
use crate::check::ty::Ty;

/// The words of an entry's state.
mod word {
    pub const DIRECTORY: u64 = 0;
    pub const TEXT: u64 = 8;
    pub const TEXT_LEN: u64 = 16;
    pub const FLAGS: u64 = 24;
}

/// The render budget of a log record's value.
const LOG_BUDGET: u64 = 1_024;

/// `hw::mint_root()`: a new root grant of the target, a `host::World`, `boot::Kernel` or
/// `boot::Machine`.
pub fn mint_root(body: &mut Body) -> Held {
    let grant = body.lo.root_grant();
    let layout = body.lo.layout(&grant);
    let slot = body.fx.slot(layout.size, layout.align);
    body.lo.call_routine(body.fx, Routine::MintRoot, &[slot]);
    Held::Address(slot)
}

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    let owner = supply
        .path
        .rsplit_once('.')
        .map_or(supply.path.as_str(), |(owner, _)| owner);
    match owner {
        "sys::Fs" => fs(lo, fx, supply),
        "sys::Net" => net(lo, fx, supply),
        "sys::Clock" => clock(lo, fx, supply),
        "sys::Random" => random(lo, fx, supply),
        "sys::Log" => log(lo, fx, supply),
        "sys::Stdio" => stdio(lo, fx, supply),
        "sys::fs::File" | "sys::net::Listener" | "sys::net::Conn" => resource(lo, fx, supply),
        "sys::revocable" | "sys::Revoker" => revocation(lo, fx, supply),
        "host_platform::submit" => submit(lo, fx, supply),
        _ => return false,
    }
    true
}

/// The entry record of the capability held at an address, failing the operation with
/// `Revoked.Stale` when its handle is stale.
fn checked(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, capability: Value) -> Value {
    let handle = fx.load(PTR, capability, 0);
    let record = live_entry(lo, fx, handle);
    let stale = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
    fx.when(stale, |fx| fail_with(lo, fx, supply, "Revoked"));
    record
}

/// Fails with a failure value of the operation's failure type: the variant of that name, or
/// for a stale handle `Revoked.Stale` itself or the `Revoked` variant holding it.
pub fn fail_with(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, variant: &str) {
    let fails = supply.abi.fails.clone().expect("a failing operation");
    let value = if variant == "Revoked" {
        let revoked = lo.std_ty("sys", "Revoked", Vec::new());
        let stale = lo.variant_value(fx, &revoked, "Stale", &[], None);
        if fails == revoked {
            stale
        } else {
            lo.variant_value(fx, &fails, "Revoked", &[stale], None)
        }
    } else {
        lo.variant_value(fx, &fails, variant, &[], None)
    };
    let failure = supply.failure.expect("a failure address");
    lo.store_held(fx, failure, value, &fails);
    supply.fail(fx);
}

/// Ends the operation after a host platform call: its failure, when it failed, becomes the
/// operation's, and `then` returns its result otherwise.
fn after(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    called: &Called,
    then: impl FnOnce(&mut Lowering, &mut Fx, Held),
) {
    supply.forward_failure(lo, fx, called);
    then(lo, fx, called.result);
}

/// A view held in memory: its address and length.
pub fn view(fx: &mut Fx, held: Held) -> (Value, Value) {
    let address = held.address();
    (fx.load(PTR, address, 0), fx.load(PTR, address, 8))
}

/// A new view value of `len` bytes from `start`.
fn new_view(fx: &mut Fx, start: Value, len: Value) -> Held {
    let slot = fx.slot(16, 8);
    fx.store(start, slot, 0);
    fx.store(len, slot, 8);
    Held::Address(slot)
}

/// A copy of `len` bytes from `start` in runtime memory, as an entry's state keeps texts.
fn kept_text(lo: &mut Lowering, fx: &mut Fx, start: Value, len: Value) -> Value {
    let size = fx.b.ins().iadd_imm_s(len, 1);
    let copy = lo.record_alloc(fx, size);
    let failed = fx.b.ins().icmp_imm_s(IntCC::Equal, copy, 0);
    fx.when(failed, |fx| {
        super::super::runtime::report::raise_kind(lo, fx, "trap.exhausted");
    });
    lo.call_routine(fx, Routine::Copy, &[copy, start, len]);
    copy
}

/// Whether a path has the form a `sys::Fs` path has: relative segments separated by `/`, none
/// empty, `.`, `..` or holding a NUL.
fn valid_path(fx: &mut Fx, start: Value, len: Value) -> Value {
    let zero = fx.ptr(0);
    let valid = fx.int(types::I8, 1);
    let state = fx.repeat(&[zero, zero, zero, valid], |fx, state| {
        let (index, segment, dots, valid) = (state[0], state[1], state[2], state[3]);
        let inside = fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, index, len);
        let going = fx.b.ins().band(inside, valid);
        let next = fx.choose(
            going,
            &[PTR, PTR, PTR, types::I8],
            |fx| {
                let at_end = fx.b.ins().icmp(IntCC::Equal, index, len);
                let byte = fx.choose(
                    at_end,
                    &[types::I8],
                    |fx| vec![fx.int(types::I8, i64::from(b'/'))],
                    |fx| {
                        let address = fx.b.ins().iadd(start, index);
                        vec![fx.load(types::I8, address, 0)]
                    },
                )[0];
                let separator = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'/'));
                let nul = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, 0);
                let dot = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'.'));
                let empty = fx.b.ins().icmp_imm_s(IntCC::Equal, segment, 0);
                let all_dots = fx.b.ins().icmp(IntCC::Equal, dots, segment);
                let short =
                    fx.b.ins()
                        .icmp_imm_s(IntCC::UnsignedLessThanOrEqual, segment, 2);
                let dotted = fx.b.ins().band(all_dots, short);
                let bad_end = fx.b.ins().bor(empty, dotted);
                let bad_end = fx.b.ins().band(separator, bad_end);
                let bad = fx.b.ins().bor(bad_end, nul);
                let still = fx.b.ins().bxor_imm_s(bad, 1);
                let valid = fx.b.ins().band(valid, still);
                let grown = fx.b.ins().iadd_imm_s(segment, 1);
                let zero = fx.ptr(0);
                let segment = fx.b.ins().select(separator, zero, grown);
                let more_dots = fx.b.ins().iadd_imm_s(dots, 1);
                let dots = fx.b.ins().select(dot, more_dots, dots);
                let dots = fx.b.ins().select(separator, zero, dots);
                vec![fx.b.ins().iadd_imm_s(index, 1), segment, dots, valid]
            },
            |_| vec![index, segment, dots, valid],
        );
        (going, next)
    });
    state[3]
}

fn fs(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let capability = supply.param(0).address();
    let record = checked(lo, fx, supply, capability);
    let directory = fx.load(PTR, record, entry::STATE + word::DIRECTORY);
    let directory = fx.b.ins().ireduce(types::I32, directory);
    let prefix_start = fx.load(PTR, record, entry::STATE + word::TEXT);
    let prefix_len = fx.load(PTR, record, entry::STATE + word::TEXT_LEN);
    let prefix = new_view(fx, prefix_start, prefix_len);
    let flags = fx.load(PTR, record, entry::STATE + word::FLAGS);
    let member = supply.member();
    if matches!(member, "create" | "write_all" | "remove") {
        let read_only = fx.b.ins().icmp_imm_s(IntCC::NotEqual, flags, 0);
        fx.when(read_only, |fx| fail_with(lo, fx, supply, "Denied"));
    }
    if matches!(
        member,
        "subdir" | "open" | "create" | "read_all" | "write_all" | "remove"
    ) {
        let (start, len) = view(fx, supply.param(1));
        let valid = valid_path(fx, start, len);
        let invalid = fx.b.ins().bxor_imm_s(valid, 1);
        fx.when(invalid, |fx| fail_with(lo, fx, supply, "InvalidPath"));
    }
    let directory = Held::Scalar(directory);
    if lo.seeded() && !matches!(member, "subdir" | "read_only") {
        match member {
            "open" | "create" => sims::fs::open(lo, fx, supply, record, member == "create"),
            "read_all" => sims::fs::read_all(lo, fx, supply, record),
            "write_all" => sims::fs::write_all(lo, fx, supply, record),
            "remove" => sims::fs::remove(lo, fx, supply, record),
            member => panic!("no lowering of sys::Fs.{member}"),
        }
        return;
    }
    match member {
        "subdir" | "read_only" => {
            let handle = fx.load(PTR, capability, 0);
            let kind = fx.ptr(kind::FS as u64);
            let derived = lo.call_routine(fx, Routine::Derive, &[handle, kind])[0];
            let stale = fx.b.ins().icmp_imm_s(IntCC::Equal, derived, -1);
            fx.when(stale, |fx| fail_with(lo, fx, supply, "Revoked"));
            let record = super::super::runtime::tables::entry_at(lo, fx, derived);
            if member == "subdir" {
                let (start, len) = view(fx, supply.param(1));
                let joined = fx.b.ins().iadd(prefix_len, len);
                let has_prefix = fx.b.ins().icmp_imm_s(IntCC::NotEqual, prefix_len, 0);
                let separator = fx.b.ins().uextend(PTR, has_prefix);
                let joined = fx.b.ins().iadd(joined, separator);
                let size = fx.b.ins().iadd_imm_s(joined, 1);
                let text = lo.record_alloc(fx, size);
                lo.call_routine(fx, Routine::Copy, &[text, prefix_start, prefix_len]);
                fx.when(has_prefix, |fx| {
                    let at = fx.b.ins().iadd(text, prefix_len);
                    let slash = fx.int(types::I8, i64::from(b'/'));
                    fx.store(slash, at, 0);
                });
                let tail = fx.b.ins().iadd(text, prefix_len);
                let tail = fx.b.ins().iadd(tail, separator);
                lo.call_routine(fx, Routine::Copy, &[tail, start, len]);
                fx.store(text, record, entry::STATE + word::TEXT);
                fx.store(joined, record, entry::STATE + word::TEXT_LEN);
            } else {
                let one = fx.ptr(1);
                fx.store(one, record, entry::STATE + word::FLAGS);
            }
            let sret = supply.sret.expect("a capability's address");
            fx.store(derived, sret, 0);
            supply.done(lo, fx);
        }
        "open" | "create" => {
            let name = match member {
                "open" => "fs_open",
                _ => "fs_create",
            };
            let called = lo.platform_call(fx, name, &[directory, prefix, supply.param(1)]);
            after(lo, fx, supply, &called, |lo, fx, fd| {
                let handle = fx.load(PTR, capability, 0);
                let fd = fx.b.ins().sextend(PTR, fd.scalar());
                ret_resource(lo, fx, supply, handle, fd);
            });
        }
        "read_all" => {
            let args = [directory, prefix, supply.param(1), supply.param(2)];
            let called = lo.platform_call(fx, "fs_read_all", &args);
            after(lo, fx, supply, &called, |lo, fx, bytes| {
                supply.ret(lo, fx, bytes)
            });
        }
        "write_all" => {
            let args = [directory, prefix, supply.param(1), supply.param(2)];
            let called = lo.platform_call(fx, "fs_write_all", &args);
            after(lo, fx, supply, &called, |lo, fx, _| supply.done(lo, fx));
        }
        "remove" => {
            let called = lo.platform_call(fx, "fs_remove", &[directory, prefix, supply.param(1)]);
            after(lo, fx, supply, &called, |lo, fx, _| supply.done(lo, fx));
        }
        member => panic!("no lowering of sys::Fs.{member}"),
    }
}

/// Returns a file, listener or connection: the entry it was opened through and the holding of
/// a descriptor or sim, which settlement closes, or abandons for a connection.
pub fn ret_resource(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    handle: Value,
    descriptor: Value,
) {
    let sret = supply.sret.expect("a resource's address");
    fx.store(handle, sret, 0);
    let linear = lo.ck.is_std(&supply.abi.result, "sys::net::Conn");
    let kind = if linear {
        holdings::kind::ABANDON
    } else {
        holdings::kind::CLOSE
    };
    let kind = fx.ptr(kind as u64);
    let holding = lo.call_routine(fx, Routine::Hold, &[kind, descriptor])[0];
    fx.store(holding, sret, 8);
    supply.done(lo, fx);
}

/// The descriptor of the resource at an address.
fn descriptor(fx: &mut Fx, address: Value) -> Value {
    let holding = fx.offset(address, 8);
    let descriptor = holdings::payload(fx, holding);
    fx.b.ins().ireduce(types::I32, descriptor)
}

fn net(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let capability = supply.param(0).address();
    let record = checked(lo, fx, supply, capability);
    let host_start = fx.load(PTR, record, entry::STATE + word::TEXT);
    let host_len = fx.load(PTR, record, entry::STATE + word::TEXT_LEN);
    let restricted = fx.load(PTR, record, entry::STATE + word::FLAGS);
    let restricted = fx.b.ins().icmp_imm_s(IntCC::NotEqual, restricted, 0);
    match supply.member() {
        "only_host" => {
            let handle = fx.load(PTR, capability, 0);
            let kind = fx.ptr(kind::NET as u64);
            let derived = lo.call_routine(fx, Routine::Derive, &[handle, kind])[0];
            let stale = fx.b.ins().icmp_imm_s(IntCC::Equal, derived, -1);
            fx.when(stale, |fx| fail_with(lo, fx, supply, "Revoked"));
            let record = super::super::runtime::tables::entry_at(lo, fx, derived);
            let (start, len) = view(fx, supply.param(1));
            let text = kept_text(lo, fx, start, len);
            fx.store(text, record, entry::STATE + word::TEXT);
            fx.store(len, record, entry::STATE + word::TEXT_LEN);
            let one = fx.ptr(1);
            fx.store(one, record, entry::STATE + word::FLAGS);
            let sret = supply.sret.expect("a capability's address");
            fx.store(derived, sret, 0);
            supply.done(lo, fx);
        }
        "listen" => {
            fx.when(restricted, |fx| fail_with(lo, fx, supply, "Denied"));
            if lo.seeded() {
                sims::net::listen(lo, fx, supply, record);
                return;
            }
            let called = lo.platform_call(fx, "net_listen", &[supply.param(1)]);
            after(lo, fx, supply, &called, |lo, fx, fd| {
                let handle = fx.load(PTR, capability, 0);
                let fd = fx.b.ins().sextend(PTR, fd.scalar());
                ret_resource(lo, fx, supply, handle, fd);
            });
        }
        "connect" => {
            let host = supply.param(1);
            fx.when(restricted, |fx| {
                let stored = new_view(fx, host_start, host_len);
                let same = lo.views_equal(fx, stored, host);
                let other = fx.b.ins().bxor_imm_s(same, 1);
                fx.when(other, |fx| fail_with(lo, fx, supply, "Denied"));
            });
            if lo.seeded() {
                sims::net::connect(lo, fx, supply, record);
                return;
            }
            let called = lo.platform_call(fx, "net_connect", &[host, supply.param(2)]);
            after(lo, fx, supply, &called, |lo, fx, fd| {
                let handle = fx.load(PTR, capability, 0);
                let fd = fx.b.ins().sextend(PTR, fd.scalar());
                ret_resource(lo, fx, supply, handle, fd);
            });
        }
        member => panic!("no lowering of sys::Net.{member}"),
    }
}

fn clock(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let capability = supply.param(0).address();
    let handle = fx.load(PTR, capability, 0);
    let record = live_entry(lo, fx, handle);
    let stale = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
    match supply.member() {
        "elapsed_ms" => {
            fx.when(stale, |fx| fail_with(lo, fx, supply, "Revoked"));
            let minted = fx.load(PTR, record, entry::STATE);
            let now = lo.now_ms(fx);
            let elapsed = fx.b.ins().isub(now, minted);
            supply.ret(lo, fx, Held::Scalar(elapsed));
        }
        "sleep" => {
            fx.when(stale, |fx| supply.done(lo, fx));
            if lo.seeded() {
                sims::sleep(lo, fx, supply, supply.param(1).scalar());
                return;
            }
            let called = lo.platform_call(fx, "sleep", &[supply.param(1)]);
            after(lo, fx, supply, &called, |lo, fx, _| supply.done(lo, fx));
        }
        member => panic!("no lowering of sys::Clock.{member}"),
    }
}

fn random(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let capability = supply.param(0).address();
    let record = checked(lo, fx, supply, capability);
    match lo.seeded() {
        true => sims::random_fill(lo, fx, record, supply.param(1)),
        false => {
            lo.platform_call(fx, "random_fill", &[supply.param(1)]);
        }
    }
    supply.done(lo, fx);
}

fn log(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let capability = supply.param(0).address();
    let handle = fx.load(PTR, capability, 0);
    let record = live_entry(lo, fx, handle);
    let live = fx.b.ins().icmp_imm_s(IntCC::NotEqual, record, 0);
    let value_ty = supply.type_arg(0);
    let level = supply.member().to_owned();
    fx.when(live, |fx| {
        if lo.seeded() {
            sims::log_line(lo, fx, &level, supply.param(1), &value_ty, supply.param(2));
            return;
        }
        let (buffer, len) = render::render_value(lo, fx, &value_ty, supply.param(2), LOG_BUDGET);
        let rendering = new_view(fx, buffer, len);
        let level = lo.view_literal(fx, level.as_bytes());
        lo.platform_call(fx, "log_line", &[level, supply.param(1), rendering]);
    });
    supply.done(lo, fx);
}

fn stdio(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let capability = supply.param(0).address();
    let record = checked(lo, fx, supply, capability);
    if lo.seeded() {
        match supply.member() {
            "read_in" => sims::stdio_read(lo, fx, supply, record),
            _ => sims::stdio_write(lo, fx, supply),
        }
        return;
    }
    let called = match supply.member() {
        "read_in" => {
            let (start, len) = view(fx, supply.param(1));
            let fd = fx.int(types::I32, 0);
            let args = [Held::Scalar(fd), Held::Scalar(start), Held::Scalar(len)];
            lo.platform_call(fx, "stdio_read", &args)
        }
        member => {
            let fd = fx.int(types::I32, if member == "write_out" { 1 } else { 2 });
            lo.platform_call(fx, "stdio_write", &[Held::Scalar(fd), supply.param(1)])
        }
    };
    after(lo, fx, supply, &called, |lo, fx, result| {
        supply.ret(lo, fx, result)
    });
}

/// The operations of files, listeners and connections, through the entry each was opened by;
/// a release completes whether or not that entry is revoked.
fn resource(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let address = supply.param(0).address();
    let member = supply.member();
    let descriptor = Held::Scalar(descriptor(fx, address));
    if member == "abandon" {
        match lo.seeded() {
            true => {
                let holding = fx.offset(address, 8);
                let payload = holdings::payload(fx, holding);
                sims::abandon(lo, fx, payload);
            }
            false => {
                lo.platform_call(fx, "conn_abandon", &[descriptor]);
            }
        }
        let holding = fx.load(PTR, address, 8);
        lo.call_routine(fx, Routine::Unhold, &[holding]);
        supply.done(lo, fx);
        return;
    }
    if member != "close" {
        checked(lo, fx, supply, address);
    }
    let owner = supply.path.rsplit_once('.').map_or("", |(owner, _)| owner);
    if lo.seeded() {
        match (owner, member) {
            ("sys::fs::File", "read") => sims::fs::read(lo, fx, supply),
            ("sys::fs::File", "write") => sims::fs::write(lo, fx, supply),
            ("sys::fs::File", "byte_len") => sims::fs::byte_len(lo, fx, supply),
            ("sys::net::Listener", "accept") => sims::net::accept(lo, fx, supply),
            ("sys::net::Conn", "read") => sims::net::read(lo, fx, supply),
            ("sys::net::Conn", "write") => sims::net::write(lo, fx, supply),
            ("sys::net::Conn", "close") => {
                sims::net::close(lo, fx, supply);
                let holding = fx.load(PTR, address, 8);
                lo.call_routine(fx, Routine::Unhold, &[holding]);
                supply.done(lo, fx);
            }
            (owner, member) => panic!("no lowering of {owner}.{member}"),
        }
        return;
    }
    let called = match (owner, member) {
        ("sys::fs::File", "read") | ("sys::net::Conn", "read") => {
            let (start, len) = view(fx, supply.param(1));
            let name = match owner {
                "sys::fs::File" => "file_read",
                _ => "conn_read",
            };
            lo.platform_call(
                fx,
                name,
                &[descriptor, Held::Scalar(start), Held::Scalar(len)],
            )
        }
        ("sys::fs::File", "write") => {
            lo.platform_call(fx, "file_write", &[descriptor, supply.param(1)])
        }
        ("sys::net::Conn", "write") => {
            lo.platform_call(fx, "conn_write", &[descriptor, supply.param(1)])
        }
        ("sys::fs::File", "byte_len") => lo.platform_call(fx, "file_byte_len", &[descriptor]),
        ("sys::net::Listener", "accept") => {
            let called = lo.platform_call(fx, "listener_accept", &[descriptor]);
            after(lo, fx, supply, &called, |lo, fx, fd| {
                let handle = fx.load(PTR, address, 0);
                let fd = fx.b.ins().sextend(PTR, fd.scalar());
                ret_resource(lo, fx, supply, handle, fd);
            });
            return;
        }
        ("sys::net::Conn", "close") => {
            let called = lo.platform_call(fx, "conn_close", &[descriptor]);
            let holding = fx.load(PTR, address, 8);
            lo.call_routine(fx, Routine::Unhold, &[holding]);
            called
        }
        (owner, member) => panic!("no lowering of {owner}.{member}"),
    };
    after(lo, fx, supply, &called, |lo, fx, result| {
        supply.ret(lo, fx, result)
    });
}

/// Releases a file or listener: its descriptor closes.
pub fn release(lo: &mut Lowering, fx: &mut Fx, path: &str, _ty: &Ty, address: Value) -> bool {
    match path {
        "sys::fs::File" | "sys::net::Listener" => {
            match lo.seeded() {
                true => {
                    let holding = fx.offset(address, 8);
                    let payload = holdings::payload(fx, holding);
                    sims::release(lo, fx, payload);
                }
                false => {
                    let descriptor = descriptor(fx, address);
                    lo.platform_call(fx, "release", &[Held::Scalar(descriptor)]);
                }
            }
            let holding = fx.load(PTR, address, 8);
            lo.call_routine(fx, Routine::Unhold, &[holding]);
            true
        }
        _ => false,
    }
}

/// Whether moving a sys type into another domain's reach does anything, none for a type std
/// declares in full.
pub fn transfers(path: &str) -> Option<bool> {
    match path {
        "sys::fs::File" | "sys::net::Listener" | "sys::net::Conn" | "sys::Revoker" => Some(true),
        _ => None,
    }
}

/// Moves a resource or revoker into another domain's reach: its holding passes to `owner`.
pub fn transfer(lo: &mut Lowering, fx: &mut Fx, path: &str, address: Value, owner: Value) -> bool {
    match path {
        "sys::fs::File" | "sys::net::Listener" | "sys::net::Conn" => {
            let holding = fx.offset(address, 8);
            transfer::rehome(lo, fx, holding, owner);
        }
        "sys::Revoker" => transfer::rehome(lo, fx, address, owner),
        _ => return false,
    }
    true
}

/// Whether releasing a sys type does anything, none for a type std declares in full.
pub fn needs_release(path: &str) -> Option<bool> {
    match path {
        "sys::fs::File" | "sys::net::Listener" => Some(true),
        "sys::net::Conn" | "sys::Revoker" => Some(false),
        _ => None,
    }
}

/// `sys::revocable`, `sys::Revoker.capability` and `sys::Revoker.revoke`: an allocator's
/// revoker keeps a new cell, any other capability's a new entry deriving from its entry.
fn revocation(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let capability = supply.type_arg(0);
    let alloc = lo.ck.is_std(&capability, "mem::Alloc");
    match supply.member() {
        "" => {
            let capability = supply.param(0).address();
            let handle = fx.load(PTR, capability, 0);
            let derived = if alloc {
                let unlimited = fx.int(PTR, -1);
                lo.call_routine(fx, Routine::Split, &[handle, unlimited])[0]
            } else {
                let record = checked(lo, fx, supply, capability);
                let kind = fx.load(types::I32, record, entry::KIND);
                let kind = fx.b.ins().uextend(PTR, kind);
                let derived = lo.call_routine(fx, Routine::Derive, &[handle, kind])[0];
                let stale = fx.b.ins().icmp_imm_s(IntCC::Equal, derived, -1);
                fx.when(stale, |fx| fail_with(lo, fx, supply, "Revoked"));
                derived
            };
            let kind = if alloc {
                holdings::kind::CLOSE_CELL
            } else {
                holdings::kind::REVOKE
            };
            let kind = fx.ptr(kind as u64);
            let holding = lo.call_routine(fx, Routine::Hold, &[kind, derived])[0];
            let sret = supply.sret.expect("a revoker's address");
            fx.store(holding, sret, 0);
            supply.done(lo, fx);
        }
        "capability" => {
            let handle = holdings::payload(fx, supply.param(0).address());
            let sret = supply.sret.expect("a capability's address");
            fx.store(handle, sret, 0);
            supply.done(lo, fx);
        }
        "revoke" => {
            let revoker = supply.param(0).address();
            let handle = holdings::payload(fx, revoker);
            let routine = if alloc {
                Routine::Close
            } else {
                Routine::Revoke
            };
            lo.call_routine(fx, routine, &[handle]);
            let holding = fx.load(PTR, revoker, 0);
            lo.call_routine(fx, Routine::Unhold, &[holding]);
            supply.done(lo, fx);
        }
        member => panic!("no lowering of sys::Revoker.{member}"),
    }
}

/// `host_platform::submit(entry)`: the operation's result once its completion arrives.
fn submit(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let entry = supply.param(0).address();
    let results = lo.call_routine(fx, Routine::IoSubmit, &[entry]);
    let (cancelled, result) = (results[0], results[1]);
    fx.when(cancelled, |fx| {
        let origin = super::tasks::origin(lo, fx);
        super::tasks::fail_cancelled(lo, fx, supply, origin);
    });
    supply.ret(lo, fx, Held::Scalar(result));
}
