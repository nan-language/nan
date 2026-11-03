//! Jobs: the entry of a nanos program's bundle, which its kernel calls with the address of a
//! `nanos::World` it gives up, that of the `mem::Bytes` of the job's input it gives up and that
//! of the `nanos::Ended` the job writes. The entry owns extended state of its own for the call,
//! runs the program's entry function through the adapter of its form, and fits the stack its
//! entry function's `limits stack N` states, when it states one.

use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature};
use cranelift_module::{FuncId, Linkage, Module};

use super::body::Held;
use super::call::Invoked;
use super::fx::PTR;
use super::stack::{self, Demand, Judged, Report};
use super::{Instance, Lowering, lines};
use crate::check::db::ItemId;
use crate::check::ty::{Arg, Scalar, Ty, no_args};
use crate::nir::{Callable, FnRef, Role, Span};

/// The symbol of a bundle's job entry.
pub const JOB: &str = "_NSjob";

/// The position among an adapter's parameters of the entry function it runs.
const MAIN: usize = 2;

impl Lowering<'_, '_> {
    /// Defines the job entry running the entry function `entry`, giving it with the stack that
    /// entry function's `limits stack N` states.
    pub fn job_entry(&mut self, entry: ItemId) -> (FuncId, Option<u64>) {
        let sig = self.ck.fn_sig(entry).expect("an entry function's head");
        let stack = sig.limits.stack;
        let fails = sig.fails.clone().expect("an entry's failure");
        let (adapter, args) = match &sig.params[..] {
            [_, input] => ("typed", vec![input.ty.clone(), sig.result.clone(), fails]),
            _ if sig.result == Ty::Scalar(Scalar::U8) => ("hosted", vec![fails]),
            _ => ("hosted_unit", vec![fails]),
        };
        let adapter = Instance::Nir {
            item: self
                .ck
                .db
                .std_item("nanos", adapter)
                .expect("the job adapter"),
            role: Role::Body,
            index: 0,
            args: args.into_iter().map(Arg::Ty).collect(),
        };
        let main = FnRef {
            callable: Callable::Item {
                item: entry,
                role: Role::Body,
                index: 0,
            },
            args: no_args(),
        };
        let mut signature = Signature::new(super::CALL_CONV);
        signature.params.extend([AbiParam::new(PTR); 3]);
        let id = self
            .module
            .declare_function(JOB, Linkage::Local, &signature)
            .expect("the job entry");
        self.define(id, signature, lines::HIDDEN, |lo, fx| {
            let params = fx.entry();
            let owned = lo.own_extended(fx);
            let inner = lo.callable_instance(&main, true);
            let glue = lo.instance(Instance::Value(Box::new(inner)));
            let reference = lo.func_ref(fx, glue);
            let code = fx.b.ins().func_addr(PTR, reference);
            let value = fx.slot(16, 8);
            fx.store(code, value, 0);
            let none = fx.ptr(0);
            fx.store(none, value, 8);
            let abi = lo.abi(&adapter);
            let callee = lo.instance(adapter.clone());
            let args = [
                Held::Address(params[0]),
                Held::Address(params[1]),
                Held::Address(value),
            ];
            let called = lo.invoke(fx, Invoked::Bound(callee, MAIN, glue), &abi, &args);
            let ended = abi.result.clone();
            lo.store_held(fx, params[2], called.result, &ended);
            lo.disown_extended(fx, owned);
            fx.b.ins().return_(&[]);
        });
        if let Some(stack) = stack {
            let head = stack::At {
                item: entry,
                span: Span::at(self.ck.db.item(entry).ast.token),
            };
            self.demands.push(Demand {
                judged: Judged::Function(id),
                stack,
                report: Report::Own { head },
            });
        }
        (id, stack)
    }
}
