//! Loading: `boot::Kernel.loader`, the pinned versions of a loaded registry's table, a loaded
//! bundle's job entry, its code's address and its stack, and the export table, runtime memory, window entries, translation invalidation and instruction
//! visibility the loader of `sys::load` reads, with the user-mode runtime's image, the entries
//! mapping an isolated space and the running root its tables share.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, condcodes::IntCC};

use super::super::Lowering;
use super::super::body::Held;
use super::super::fx::{Fx, PTR};
use super::super::machine::Stub;
use super::super::runtime::{Routine, fetch_add, holdings, report};
use super::{Supply, tasks as task_ops};
use crate::workspace::Arch;

/// The offset in a version of the count of reads holding it.
const READERS: u64 = 16;

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    match supply.path.as_str() {
        "boot::Kernel.loader" => {
            let option = supply.abi.result.clone();
            let loader = match lo.exports() {
                true => lo.variant_value(fx, &option, "Some", &[Held::Empty], None),
                false => lo.none_value(fx, &option),
            };
            supply.ret(lo, fx, loader);
        }
        "sys::load::Job.at" => {
            let sret = supply.sret.expect("a job's address");
            fx.store(supply.param(0).scalar(), sret, 0);
            fx.store(supply.param(1).scalar(), sret, 8);
            supply.ret(lo, fx, Held::Address(sret));
        }
        "sys::load::Job.stack" => {
            let stack = fx.load(PTR, supply.param(0).address(), 8);
            supply.ret(lo, fx, Held::Scalar(stack));
        }
        "sys::load::Table.record" => {
            let table = supply.param(0).address();
            let record = fx.load(PTR, table, 0);
            supply.ret(lo, fx, Held::Scalar(record));
        }
        "sys::load::Table.pinned" => {
            let holding = supply.param(1).scalar();
            let readers = fx.load(PTR, holding, holdings::holding::PAYLOAD);
            let version = fx.b.ins().iadd_imm_s(readers, -(READERS as i64));
            let entries = fx.load(PTR, version, 0);
            let len = fx.load(PTR, version, 8);
            let sret = supply.sret.expect("a view's address");
            fx.store(entries, sret, 0);
            fx.store(len, sret, 8);
            supply.done(lo, fx);
        }
        "sys::load::pin" => {
            let record = supply.param(0).scalar();
            let pin = fx.ptr(holdings::kind::PIN as u64);
            let zero = fx.ptr(0);
            let holding = lo.call_routine(fx, Routine::Hold, &[pin, zero])[0];
            let found = fx.repeat(&[zero], |fx, _| {
                let version = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), record);
                let readers = fx.offset(version, READERS);
                let one = fx.ptr(1);
                fetch_add(fx, readers, one);
                let again = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), record);
                let moved = fx.b.ins().icmp(IntCC::NotEqual, again, version);
                fx.when(moved, |fx| {
                    let less = fx.int(PTR, -1);
                    fetch_add(fx, readers, less);
                });
                (moved, vec![readers])
            });
            fx.store(found[0], holding, holdings::holding::PAYLOAD);
            supply.ret(lo, fx, Held::Scalar(holding));
        }
        "sys::load::unpin" => {
            let holding = supply.param(0).scalar();
            let readers = fx.load(PTR, holding, holdings::holding::PAYLOAD);
            lo.call_routine(fx, Routine::Unhold, &[holding]);
            let less = fx.int(PTR, -1);
            fetch_add(fx, readers, less);
            supply.done(lo, fx);
        }
        "sys::load::exports" => {
            let table = lo.export_table(fx);
            supply.ret(lo, fx, Held::Scalar(table));
        }
        "sys::load::reserve" => {
            let address = lo.record_alloc(fx, supply.param(0).scalar());
            let none = fx.b.ins().icmp_imm_s(IntCC::Equal, address, 0);
            fx.when(none, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
            supply.ret(lo, fx, Held::Scalar(address));
        }
        "sys::load::release" => {
            lo.record_free(fx, supply.param(0).scalar());
            supply.done(lo, fx);
        }
        "sys::load::Loader.runtime" => {
            let image = lo.runtime_image.clone().unwrap_or_default();
            let view = lo.view_literal(fx, &image);
            supply.ret(lo, fx, view);
        }
        "sys::load::user_entry" => {
            let (physical, kind) = (supply.param(0).scalar(), supply.param(1).scalar());
            let entry = lo.user_entry(fx, physical, kind);
            supply.ret(lo, fx, Held::Scalar(entry));
        }
        "sys::load::running_root" => {
            let root = lo.running_root(fx);
            supply.ret(lo, fx, Held::Scalar(root));
        }
        "sys::load::window_entry" => {
            let (physical, kind) = (supply.param(0).scalar(), supply.param(1).scalar());
            let entry = lo.window_entry(fx, physical, kind);
            supply.ret(lo, fx, Held::Scalar(entry));
        }
        "sys::load::invalidate" => {
            let early = task_ops::origin(lo, fx);
            let cancelled = fx.b.ins().icmp_imm_s(IntCC::NotEqual, early, 0);
            fx.when(cancelled, |fx| {
                task_ops::fail_cancelled(lo, fx, supply, early)
            });
            let all = fx.ptr(1);
            lo.invalidate_everywhere(fx, all);
            supply.done(lo, fx);
        }
        "sys::load::sync" => {
            if lo.ck.db.target.arch() == Some(Arch::Aarch64) {
                let (address, len) = (supply.param(0).scalar(), supply.param(1).scalar());
                lo.call_stub(fx, Stub::TlbiAll, &[]);
                lo.call_stub(fx, Stub::SyncInstructions, &[address, len]);
            }
            supply.done(lo, fx);
        }
        _ => return false,
    }
    true
}
