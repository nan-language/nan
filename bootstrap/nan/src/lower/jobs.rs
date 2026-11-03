//! Jobs and component entries: the entries of a nanos program's bundle, which its kernel calls with
//! the address of a `nanos::World` it gives up, that of the input it gives up, the `mem::Bytes`
//! of a job's or what the task of a component's instance receives, and that of the
//! `nanos::Ended` the entry writes. An entry owns extended state of its own for the call. The job
//! entry runs the program's entry function through the adapter of its form, and fits the stack
//! its entry function's `limits stack N` states, when it states one; a component entry runs the
//! instance's task through `nanos::Hosted.run`.

use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature, Value};
use cranelift_module::{FuncId, Linkage, Module};

use super::body::Held;
use super::call::{Called, Invoked};
use super::fx::{Fx, PTR};
use super::stack::{self, Demand, Judged, Report};
use super::{Instance, Lowering, lines};
use crate::check::db::{ItemId, TypeKey};
use crate::check::ty::{Arg, Scalar, Ty, no_args};
use crate::nir::{Callable, FnRef, Role, Span};

/// The symbol of a bundle's job entry.
pub const JOB: &str = "_NSjob";

/// The prefix of the symbol of a bundle's entry of the component of a name.
pub const COMPONENT: &str = "_NScomponent_";

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
        let id = self.adapter_entry(JOB, |lo, fx, params| {
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
            (called, abi.result)
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

    /// Defines the entry of the component `name`, whose state is `state`, running the task of
    /// its instances.
    pub fn component_entry(&mut self, name: &str, state: ItemId) -> FuncId {
        let state = Ty::item(state, Vec::new());
        let (env, protocol) = self
            .ck
            .component_of(&state)
            .expect("a component entry's Env and protocol");
        let hosted = self
            .ck
            .db
            .std_item("nanos", "Hosted")
            .expect("the component adapter");
        let adapter = Instance::Nir {
            item: self.ck.db.members(TypeKey::Item(hosted), "run")[0],
            role: Role::Body,
            index: 0,
            args: [state, env, protocol].into_iter().map(Arg::Ty).collect(),
        };
        let symbol = format!("{COMPONENT}{name}");
        self.adapter_entry(&symbol, |lo, fx, params| {
            let abi = lo.abi(&adapter);
            let callee = lo.instance(adapter.clone());
            let args = [Held::Address(params[0]), Held::Address(params[1])];
            let called = lo.invoke(fx, Invoked::Direct(callee), &abi, &args);
            (called, abi.result)
        })
    }

    /// Defines an entry of a bundle under `symbol`, taking the addresses of a world, of an input
    /// and of the `nanos::Ended` it writes, which owns extended state while `call` calls an
    /// adapter with them, and writes the adapter's result of the type given.
    fn adapter_entry(
        &mut self,
        symbol: &str,
        call: impl FnOnce(&mut Self, &mut Fx, &[Value]) -> (Called, Ty),
    ) -> FuncId {
        let mut signature = Signature::new(super::CALL_CONV);
        signature.params.extend([AbiParam::new(PTR); 3]);
        let id = self
            .module
            .declare_function(symbol, Linkage::Local, &signature)
            .expect("a bundle's entry");
        self.define(id, signature, lines::HIDDEN, |lo, fx| {
            let params = fx.entry();
            let owned = lo.own_extended(fx);
            let (called, ended) = call(lo, fx, &params);
            lo.store_held(fx, params[2], called.result, &ended);
            lo.disown_extended(fx, owned);
            fx.b.ins().return_(&[]);
        });
        id
    }
}
