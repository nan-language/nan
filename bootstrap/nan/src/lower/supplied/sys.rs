//! System capabilities: the root grant, the operations of `sys::Fs`, `sys::Net`, `sys::Clock`,
//! `sys::WallClock`, `sys::Random`, `sys::Log` and `sys::Stdio` through the host platform, or in a test image
//! through the sims, files, listeners and connections, revocation, and the submission of
//! io_uring operations.
//!
//! Each operation checks its capability's entry first, reading its state words through a
//! snapshot and its text through a copy. An `Fs` entry's state is its directory's descriptor,
//! the address and length of its path prefix, and whether it is read-only; a `Net` entry's is
//! the address and length of the one host it reaches, when restricted. A file,
//! listener or connection is the entry it was opened through and the holding of its
//! descriptor; a revoker is the holding of its entry or cell.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::body::{Body, Held};
use super::super::call::Called;
use super::super::fx::{Fx, PTR};
use super::super::runtime::tables::{self, RESOLUTION, entry, live_entry};
use super::super::runtime::{Routine, holdings, seeded, timers};
use super::super::transfer;
use super::Supply;
use super::render;
use super::sims;
use crate::check::ty::Ty;

/// The state word of an `Fs` entry holding whether it is read-only, and of a `Net` entry
/// whether it is restricted.
const FLAGS: u64 = entry::STATE + 24;

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
        "sys::WallClock" => wall_clock(lo, fx, supply),
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

/// Fails the operation with `Revoked.Stale` when the handle of the capability held at an
/// address is stale.
fn check(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, capability: Value) {
    let handle = fx.load(PTR, capability, 0);
    let record = live_entry(lo, fx, handle);
    let stale = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
    fx.when(stale, |fx| fail_with(lo, fx, supply, "Revoked"));
}

/// The state words of the entry of the capability held at an address, failing the operation
/// with `Revoked.Stale` when its handle is stale.
fn checked(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, capability: Value) -> [Value; 4] {
    let handle = fx.load(PTR, capability, 0);
    let (live, words) = tables::snapshot(lo, fx, handle);
    let stale = fx.b.ins().bxor_imm_s(live, 1);
    fx.when(stale, |fx| fail_with(lo, fx, supply, "Revoked"));
    words
}

/// A copy of the text of the entry of the capability held at an address, which the caller
/// frees, failing the operation with `Revoked.Stale` when its handle is stale.
pub fn checked_text(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    capability: Value,
) -> (Value, Value) {
    let handle = fx.load(PTR, capability, 0);
    let (copy, len) = tables::copy_text(lo, fx, handle);
    let stale = fx.b.ins().icmp_imm_s(IntCC::Equal, copy, 0);
    fx.when(stale, |fx| fail_with(lo, fx, supply, "Revoked"));
    (copy, len)
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
    let [directory, _, _, flags] = checked(lo, fx, supply, capability);
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
    match member {
        "subdir" => {
            let (start, len) = view(fx, supply.param(1));
            let floor = fx.ptr(0);
            attenuate(lo, fx, supply, FLAGS, floor, Some((start, len)));
            return;
        }
        "read_only" => {
            let floor = fx.ptr(1);
            attenuate(lo, fx, supply, FLAGS, floor, None);
            return;
        }
        _ => {}
    }
    if lo.seeded() {
        match member {
            "open" | "create" => sims::fs::open(lo, fx, supply, directory, member == "create"),
            "read_all" => sims::fs::read_all(lo, fx, supply, directory),
            "write_all" => sims::fs::write_all(lo, fx, supply, directory),
            "remove" => sims::fs::remove(lo, fx, supply, directory),
            member => panic!("no lowering of sys::Fs.{member}"),
        }
        return;
    }
    let prefix = checked_text(lo, fx, supply, capability);
    let directory = fx.b.ins().ireduce(types::I32, directory);
    let directory = Held::Scalar(directory);
    let path = new_view(fx, prefix.0, prefix.1);
    let (name, args) = match member {
        "open" => ("fs_open", vec![directory, path, supply.param(1)]),
        "create" => ("fs_create", vec![directory, path, supply.param(1)]),
        "read_all" => (
            "fs_read_all",
            vec![directory, path, supply.param(1), supply.param(2)],
        ),
        "write_all" => (
            "fs_write_all",
            vec![directory, path, supply.param(1), supply.param(2)],
        ),
        "remove" => ("fs_remove", vec![directory, path, supply.param(1)]),
        member => panic!("no lowering of sys::Fs.{member}"),
    };
    let called = lo.platform_call(fx, name, &args);
    lo.record_free(fx, prefix.0);
    after(lo, fx, supply, &called, |lo, fx, result| match member {
        "open" | "create" => {
            let handle = fx.load(PTR, capability, 0);
            let fd = fx.b.ins().sextend(PTR, result.scalar());
            ret_resource(lo, fx, supply, handle, fd);
        }
        "read_all" => supply.ret(lo, fx, result),
        _ => supply.done(lo, fx),
    });
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
    let [network, _, _, restricted] = checked(lo, fx, supply, capability);
    let restricted = fx.b.ins().icmp_imm_s(IntCC::NotEqual, restricted, 0);
    match supply.member() {
        "only_host" => {
            let host = view(fx, supply.param(1));
            let floor = fx.ptr(1);
            attenuate(lo, fx, supply, FLAGS, floor, Some(host));
        }
        "listen" => {
            fx.when(restricted, |fx| fail_with(lo, fx, supply, "Denied"));
            if lo.seeded() {
                sims::net::listen(lo, fx, supply, network);
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
                let (copy, len) = checked_text(lo, fx, supply, capability);
                let stored = new_view(fx, copy, len);
                let same = lo.views_equal(fx, stored, host);
                lo.record_free(fx, copy);
                let other = fx.b.ins().bxor_imm_s(same, 1);
                fx.when(other, |fx| fail_with(lo, fx, supply, "Denied"));
            });
            if lo.seeded() {
                sims::net::connect(lo, fx, supply, network);
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
    let (live, [minted, resolution, _, _]) = tables::snapshot(lo, fx, handle);
    let stale = fx.b.ins().bxor_imm_s(live, 1);
    match supply.member() {
        member @ ("elapsed_ms" | "elapsed_ns") => {
            fx.when(stale, |fx| fail_with(lo, fx, supply, "Revoked"));
            let now = lo.now_ns(fx);
            let elapsed = fx.b.ins().isub(now, minted);
            let elapsed = round_down(fx, resolution, elapsed);
            let elapsed = match member {
                "elapsed_ms" => {
                    let per_ms = fx.ptr(timers::NS_PER_MS);
                    fx.b.ins().udiv(elapsed, per_ms)
                }
                _ => elapsed,
            };
            supply.ret(lo, fx, Held::Scalar(elapsed));
        }
        member @ ("sleep" | "sleep_ns") => {
            fx.when(stale, |fx| supply.done(lo, fx));
            let ns = match member {
                "sleep" => timers::from_ms(fx, supply.param(1).scalar()),
                _ => supply.param(1).scalar(),
            };
            let delay = clock_delay(lo, fx, minted, resolution, ns);
            if lo.seeded() || lo.kernel() {
                let cancelled = sims::wait(lo, fx, None, |lo, fx, op| match lo.seeded() {
                    true => seeded::arm(lo, fx, op, delay),
                    false => lo.kernel_arm(fx, op, delay),
                });
                fx.when(cancelled, |fx| sims::fail_cancelled(lo, fx, supply));
                supply.done(lo, fx);
                return;
            }
            let called = lo.platform_call(fx, "sleep", &[Held::Scalar(delay)]);
            after(lo, fx, supply, &called, |lo, fx, _| supply.done(lo, fx));
        }
        "coarse" => coarse(lo, fx, supply, RESOLUTION),
        member => panic!("no lowering of sys::Clock.{member}"),
    }
}

fn wall_clock(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    match supply.member() {
        "now_ns" => {
            let [ahead, resolution, _, _] = checked(lo, fx, supply, supply.param(0).address());
            let now = match lo.seeded() || lo.machine() {
                true => {
                    let now = lo.now_ns(fx);
                    fx.b.ins().iadd(now, ahead)
                }
                false => lo.platform_call(fx, "wall_ns", &[]).result.scalar(),
            };
            let now = round_down(fx, resolution, now);
            supply.ret(lo, fx, Held::Scalar(now));
        }
        "coarse" => coarse(lo, fx, supply, RESOLUTION),
        member => panic!("no lowering of sys::WallClock.{member}"),
    }
}

/// `c.coarse(ns)` of a `Clock`, `WallClock` or `task::Executor`: an entry deriving from `c`'s,
/// with the coarser of its resolution, held in the state word at `word`, and `ns`.
pub fn coarse(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, word: u64) {
    let ns = supply.param(1).scalar();
    attenuate(lo, fx, supply, word, ns, None);
}

/// Returns an entry deriving from the entry of the capability held at the operation's first
/// parameter: its state word at offset `word` raised to at least `floor` and its text extended
/// or replaced by `text`, failing the operation with `Revoked.Stale` when its handle is stale.
fn attenuate(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    word: u64,
    floor: Value,
    text: Option<(Value, Value)>,
) {
    let handle = fx.load(PTR, supply.param(0).address(), 0);
    let word = fx.ptr(word);
    let (text, len) = text.unwrap_or_else(|| (fx.ptr(0), fx.ptr(0)));
    let derived = lo.call_routine(fx, Routine::Derive, &[handle, word, floor, text, len])[0];
    let stale = fx.b.ins().icmp_imm_s(IntCC::Equal, derived, -1);
    fx.when(stale, |fx| fail_with(lo, fx, supply, "Revoked"));
    let sret = supply.sret.expect("a capability's address");
    fx.store(derived, sret, 0);
    supply.done(lo, fx);
}

/// A reading of a clock rounded down to a multiple of its resolution.
fn round_down(fx: &mut Fx, resolution: Value, reading: Value) -> Value {
    let one = fx.ptr(1);
    let resolution = fx.b.ins().umax(resolution, one);
    let rest = fx.b.ins().urem(reading, resolution);
    fx.b.ins().isub(reading, rest)
}

/// The delay after which `ns` nanoseconds have elapsed on the clock of a live `Clock` entry
/// minted at `minted` with a resolution, rounded up so that it ends when the clock's elapsed
/// time is a multiple of its resolution.
pub fn clock_delay(
    lo: &mut Lowering,
    fx: &mut Fx,
    minted: Value,
    resolution: Value,
    ns: Value,
) -> Value {
    let now = lo.now_ns(fx);
    timers::on_grid(fx, now, minted, resolution, ns)
}

fn random(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let capability = supply.param(0).address();
    let [stream, _, _, _] = checked(lo, fx, supply, capability);
    match lo.seeded() {
        true => sims::random_fill(lo, fx, stream, supply.param(1)),
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
    let [input, _, _, _] = checked(lo, fx, supply, capability);
    if lo.seeded() {
        match supply.member() {
            "read_in" => sims::stdio_read(lo, fx, supply, input),
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
        check(lo, fx, supply, address);
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
                check(lo, fx, supply, capability);
                let word = fx.ptr(entry::STATE);
                let zero = fx.ptr(0);
                let args = [handle, word, zero, zero, zero];
                let derived = lo.call_routine(fx, Routine::Derive, &args)[0];
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
