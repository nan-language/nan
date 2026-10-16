//! Calls: direct calls of lowered instances, calls through function values and `Dyn` values,
//! calls of foreign fns, and the glue that lets a named function be a function value and an
//! implementing function be a `Dyn` method.

use cranelift_codegen::ir::{self, AbiParam, InstBuilder, Signature, Value as IrValue, types};
use cranelift_module::{FuncId, Linkage, Module};

use super::abi::{Abi, Repr, Returned};
use super::body::{Body, Held};
use super::fx::{Fx, PTR};
use super::{Instance, Lowering, stack, supplied};
use crate::check::db::ItemId;
use crate::check::sig::SigKind;
use crate::check::ty::{Arg, Args, FnTy, Subst, Ty};
use crate::nir::{BlockId, Call, Callable, Callee, FnRef, NTy, Op, Operand, Role, Value};

/// What a call calls.
enum Target {
    Direct(FuncId, Abi),
    /// Through a function value: its code address and environment, and how its stack use is
    /// bounded.
    Indirect {
        code: IrValue,
        env: IrValue,
        abi: Abi,
        kind: stack::Kind,
    },
    /// An external symbol under the C convention of the host, with the `stack N` its head
    /// demands.
    Foreign(FuncId, Vec<Repr>, Returned, Option<u64>),
}

/// Where a function value an operand passes comes from.
enum FnSource {
    /// A closure written there, by index.
    Closure(u32),
    /// A named function written there.
    Named(FnRef),
    /// The function's own parameter at a position.
    Param(usize),
    /// Any other value, with the `stack N` its type demands.
    Other(Option<u64>),
}

impl Body<'_, '_, '_, '_> {
    /// Lowers a call: its result as `result` defines, or, for a failing call ending a block, a
    /// branch to `targets` with its result or its failure.
    pub fn call(
        &mut self,
        call: &Call,
        result: Option<Value>,
        block: BlockId,
        position: usize,
        targets: Option<(BlockId, BlockId)>,
    ) -> Option<Held> {
        let target = match &call.callee {
            Callee::Fn(fn_ref) => {
                let fn_ref = FnRef {
                    callable: fn_ref.callable.clone(),
                    args: self.subst.args(&fn_ref.args),
                };
                if let Callable::Builtin(builtin) = &fn_ref.callable {
                    let held = supplied::builtin(
                        self,
                        *builtin,
                        &fn_ref.args,
                        &call.args,
                        result,
                        block,
                        position,
                    );
                    return Some(held);
                }
                if supplied::tasks::scopes::is_spawn(self.lo, &fn_ref) {
                    return Some(supplied::tasks::scopes::spawn_at(self, &call.args));
                }
                self.bounds_check(&fn_ref, &call.args, block, position);
                supplied::numbers::operand_checks(self, &fn_ref, &call.args);
                self.direct_target(&fn_ref)
            }
            Callee::Value(value) => {
                let Ty::Fn(fn_ty) = self.nty(*value).ty().clone() else {
                    panic!("an indirect call through a value of no function type");
                };
                let function = self.held(*value).address();
                let code = self.fx.load(PTR, function, 0);
                let env = self.fx.load(PTR, function, 8);
                let abi = self.lo.fn_abi(&fn_ty);
                let kind = match self.own_param(*value) {
                    Some(position) => stack::Kind::Param(position),
                    None => stack::Kind::Bounded(fn_ty.limits.stack),
                };
                Target::Indirect {
                    code,
                    env,
                    abi,
                    kind,
                }
            }
            Callee::Dyn {
                place,
                interface,
                method,
            } => {
                let dynamic = self.place_address(place);
                let data = self.fx.load(PTR, dynamic, 0);
                let table = self.fx.load(PTR, dynamic, 8);
                let code = self.fx.load(PTR, table, 8 * (u64::from(*method) + 3));
                let dyn_ty = self.place_type(place);
                let args = match &dyn_ty {
                    Ty::Dyn(bound) => bound.args.clone(),
                    _ => self.lo.box_dyn_args(&dyn_ty),
                };
                let mut abi = self.lo.dyn_abi(*interface, &args, *method);
                abi.env = true;
                let (head, _) = self.lo.method_head(&Ty::Unit, *interface, &args, *method);
                Target::Indirect {
                    code,
                    env: data,
                    abi,
                    kind: stack::Kind::Bounded(head.limits.stack),
                }
            }
        };
        self.emit_call(target, &call.args, targets)
    }

    /// Checks the index of an `at` or `at_mut` call of an array, slice, list, bytes or bounded
    /// list against the length, when its obligation site is checked.
    fn bounds_check(
        &mut self,
        fn_ref: &FnRef,
        operands: &[Operand],
        block: BlockId,
        position: usize,
    ) {
        let Callable::Item {
            item,
            role: Role::Body,
            ..
        } = fn_ref.callable
        else {
            return;
        };
        let path = self.lo.ck.db.canonical(item);
        let Some((owner, "at" | "at_mut")) = path.rsplit_once('.') else {
            return;
        };
        if !self.checked(block, position) {
            return;
        }
        let (receiver, nty) = self.operand(&operands[0]);
        let ty = nty.ty().clone();
        let len = match owner {
            "core::Array" => {
                let count = super::data::const_arg(&ty.args()[1]) as u64;
                self.fx.ptr(count)
            }
            "core::Slice" | "core::SliceMut" | "mem::List" | "mem::Bytes" => {
                self.fx.load(PTR, receiver.address(), 8)
            }
            "mem::Bounded" => {
                let at = self.lo.layouts.field(self.lo.ck, &ty, None, "len").offset;
                self.fx.load(PTR, receiver.address(), at)
            }
            _ => return,
        };
        let (index, index_nty) = self.operand(&operands[1]);
        let index = self.adapt(index, &index_nty, Repr::Scalar(PTR)).scalar();
        let inside = self
            .fx
            .b
            .ins()
            .icmp(ir::condcodes::IntCC::UnsignedLessThan, index, len);
        let operands = super::trap::Operands::Bounds(operands[1].clone(), index, len);
        self.trap_unless(inside, "trap.bounds", operands);
    }

    /// The function a direct call reaches.
    fn direct_target(&mut self, fn_ref: &FnRef) -> Target {
        if let Callable::Item {
            item,
            role: Role::Body,
            ..
        } = fn_ref.callable
            && self.lo.ck.is_foreign(item)
        {
            let (id, params, returned) = self.lo.foreign(item);
            let stack = self.lo.ck.fn_sig(item).and_then(|sig| sig.limits.stack);
            return Target::Foreign(id, params, returned, stack);
        }
        let instance = self.lo.callable_instance(fn_ref, false);
        let abi = self.lo.abi(&instance);
        let id = self.lo.instance(instance);
        Target::Direct(id, abi)
    }

    fn emit_call(
        &mut self,
        target: Target,
        operands: &[Operand],
        targets: Option<(BlockId, BlockId)>,
    ) -> Option<Held> {
        let (abi, foreign) = match &target {
            Target::Direct(_, abi) | Target::Indirect { abi, .. } => (abi.clone(), None),
            Target::Foreign(_, params, returned, _) => (
                Abi {
                    env: false,
                    params: params
                        .iter()
                        .map(|repr| (NTy::Owned(Ty::Unit), *repr))
                        .collect(),
                    result: Ty::Unit,
                    returned: *returned,
                    fails: None,
                    flagged: false,
                },
                Some(()),
            ),
        };
        let mut args = Vec::new();
        if let Target::Indirect { env, .. } = &target {
            args.push(*env);
        }
        let result_slot = match abi.returned {
            Returned::Indirect => {
                let layout = self.lo.layout(&abi.result);
                let slot = self.fx.slot(layout.size, layout.align);
                args.push(slot);
                Some(slot)
            }
            _ => None,
        };
        let failure_slot = abi.flagged.then(|| {
            let slot = self.lo.failure_slot(self.fx, &abi);
            args.push(slot);
            slot
        });
        let passed = self.passed(operands, &abi, foreign.is_some());
        args.extend(passed);
        let inst = match &target {
            Target::Direct(id, _) | Target::Foreign(id, ..) => {
                let reference = self.lo.func_ref(self.fx, *id);
                self.fx.b.ins().call(reference, &args)
            }
            Target::Indirect { code, abi, .. } => {
                let signature = self.fx.b.import_signature(abi.signature());
                self.fx.b.ins().call_indirect(signature, *code, &args)
            }
        };
        let kind = match &target {
            Target::Direct(callee, abi) => {
                let bindings = operands
                    .iter()
                    .zip(&abi.params)
                    .enumerate()
                    .filter(|(_, (_, (nty, _)))| matches!(nty.ty(), Ty::Fn(_)))
                    .map(|(position, (operand, _))| (position, self.binding(operand)))
                    .collect();
                stack::Kind::Direct {
                    callee: *callee,
                    bindings,
                }
            }
            Target::Foreign(.., stack) => stack::Kind::Bounded(*stack),
            Target::Indirect { kind, .. } => kind.clone(),
        };
        self.record_site(inst, kind);
        let returns = self.fx.b.inst_results(inst).to_vec();
        let mut returns = returns.into_iter();
        let flag = abi.flagged.then(|| returns.next().expect("a failure flag"));
        let value = match abi.returned {
            Returned::Scalar(_) => Held::Scalar(returns.next().expect("a result")),
            Returned::Part => Held::Address(returns.next().expect("a part's address")),
            Returned::Indirect => Held::Address(result_slot.expect("a result slot")),
            Returned::Empty => Held::Empty,
        };
        if abi.result == Ty::Never && targets.is_none() {
            self.unreachable_after();
            return Some(Held::Empty);
        }
        let Some((ok, fail)) = targets else {
            return Some(value);
        };
        let flag = flag.expect("a failing call");
        let failure = failure_slot.expect("a failure slot");
        let failure_ty = abi.fails.clone().expect("a failure type");
        let failure_held = match self.lo.owned_repr(&failure_ty) {
            Repr::Scalar(_) => Held::Scalar(self.load_scalar(failure, 0, &failure_ty)),
            Repr::Address => Held::Address(failure),
            Repr::Empty => Held::Empty,
        };
        let ok_args = self.target_args(ok, value, &NTy::Owned(abi.result.clone()));
        let fail_args = self.target_args(fail, failure_held, &NTy::Owned(failure_ty));
        let ok_block = self.block_of(ok);
        let fail_block = self.block_of(fail);
        let ok_args: Vec<ir::BlockArg> = ok_args.into_iter().map(Into::into).collect();
        let fail_args: Vec<ir::BlockArg> = fail_args.into_iter().map(Into::into).collect();
        self.fx
            .b
            .ins()
            .brif(flag, fail_block, &fail_args, ok_block, &ok_args);
        None
    }

    /// The values passing operands as an ABI's parameters, as the C convention of the host
    /// passes them when `foreign`.
    fn passed(&mut self, operands: &[Operand], abi: &Abi, foreign: bool) -> Vec<IrValue> {
        let mut args = Vec::new();
        for (operand, (nty, repr)) in operands.iter().zip(&abi.params) {
            let (held, operand_nty) = self.operand(operand);
            let held = match foreign {
                true => held,
                false => {
                    let value = self.operand_value(operand);
                    self.plain(held, value, nty.ty())
                }
            };
            let held = self.adapt(held, &operand_nty, *repr);
            args.extend(held.values());
        }
        for (operand, (nty, _)) in operands.iter().zip(&abi.params) {
            self.fit(operand, nty.ty());
        }
        args
    }

    /// The record of `size` bytes keeping the two sides of the comparison a contract
    /// function's result is, computed again from the operands of a call of it.
    pub fn call_sides(&mut self, fn_ref: &FnRef, operands: &[Operand], size: u64) -> IrValue {
        let Callable::Item { item, role, index } = fn_ref.callable else {
            panic!("the sides of a call of no contract function");
        };
        let args = self.subst.args(&fn_ref.args);
        let instance = Instance::Sides(Box::new(Instance::Nir {
            item,
            role,
            index,
            args,
        }));
        let abi = self.lo.abi(&instance);
        let id = self.lo.instance(instance);
        let record = self.fx.slot(size, 8);
        let mut args = vec![record];
        args.extend(self.passed(operands, &abi, false));
        let reference = self.lo.func_ref(self.fx, id);
        self.fx.b.ins().call(reference, &args);
        record
    }

    /// Records a call site of the function, at the instruction being lowered.
    fn record_site(&mut self, inst: ir::Inst, kind: stack::Kind) {
        let at = stack::At {
            item: self.item,
            span: self.current,
        };
        self.fx
            .sites
            .push((inst, stack::Site { kind, at: Some(at) }));
    }

    /// The position among the function's parameters of a value that is one of its own
    /// function-typed parameters, a closure's captures excluded.
    fn own_param(&self, value: Value) -> Option<usize> {
        let function = self.function();
        let position = function
            .params
            .iter()
            .position(|param| param.value == value)?;
        let captures = match function.role {
            Role::Closure => self.lo.closure_captures(&self.unit, function.index),
            _ => 0,
        };
        let own = position.checked_sub(captures)?;
        matches!(self.nty(value).ty(), Ty::Fn(_)).then_some(own)
    }

    /// Where the function value an operand passes comes from: a closure or named function
    /// written there, through the temporary holding it, or one of the function's own
    /// parameters.
    fn fn_source(&mut self, operand: &Operand) -> FnSource {
        let ty = match operand {
            Operand::Value(value) => Some(self.nty(*value).ty().clone()),
            Operand::Place(_, place) => Some(self.place_type(place)),
            _ => None,
        };
        let bound = match ty {
            Some(Ty::Fn(fn_ty)) => fn_ty.limits.stack,
            _ => None,
        };
        let mut value = match operand {
            Operand::Value(value) => *value,
            Operand::Place(_, place) if place.steps.is_empty() => place.root,
            _ => return FnSource::Other(bound),
        };
        if let Some(position) = self.own_param(value) {
            return FnSource::Param(position);
        }
        for _ in 0..2 {
            match self.definition(value) {
                Some(Op::Closure { index, .. }) => return FnSource::Closure(*index),
                Some(Op::FnRef(fn_ref)) => return FnSource::Named(fn_ref.clone()),
                Some(Op::Load(place) | Op::Move(place)) if place.steps.is_empty() => {
                    match self.initializer(place.root) {
                        Some(Operand::Value(initial)) => value = *initial,
                        _ => return FnSource::Other(bound),
                    }
                }
                _ => return FnSource::Other(bound),
            }
        }
        FnSource::Other(bound)
    }

    /// The operation defining a value of the function.
    fn definition(&self, value: Value) -> Option<&Op> {
        self.function()
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .find(|inst| inst.result == Some(value))
            .map(|inst| &inst.op)
    }

    /// The operand initialising a slot, when the function initialises it once and never
    /// assigns it.
    fn initializer(&self, slot: Value) -> Option<&Operand> {
        let mut writes = self
            .function()
            .blocks
            .iter()
            .flat_map(|block| &block.insts)
            .filter_map(|inst| match &inst.op {
                Op::Init(place, operand) | Op::Assign(place, operand) if place.root == slot => {
                    Some((place, operand))
                }
                _ => None,
            });
        let (place, operand) = writes.next()?;
        (writes.next().is_none() && place.steps.is_empty()).then_some(operand)
    }

    /// What an operand passes for a function-typed parameter of a direct call's callee.
    fn binding(&mut self, operand: &Operand) -> stack::Binding {
        match self.fn_source(operand) {
            FnSource::Closure(index) => stack::Binding::Function(self.closure_function(index)),
            FnSource::Named(fn_ref) => stack::Binding::Function(self.named_function(&fn_ref)),
            FnSource::Param(position) => stack::Binding::Param(position),
            FnSource::Other(bound) => stack::Binding::Bounded(bound),
        }
    }

    /// Records the `stack N` demand a function type `wanted` makes of the function value an
    /// operand passes where it fits that type.
    pub fn fit(&mut self, operand: &Operand, wanted: &Ty) {
        let Ty::Fn(wanted) = wanted else {
            return;
        };
        let Some(stack) = wanted.limits.stack else {
            return;
        };
        let at = stack::At {
            item: self.item,
            span: self.current,
        };
        let (judged, report) = match self.fn_source(operand) {
            FnSource::Closure(index) => {
                let unit = self.unit.clone();
                let span = unit
                    .function(Role::Closure, index)
                    .expect("a closure function")
                    .span;
                let head = stack::At {
                    item: self.item,
                    span,
                };
                (
                    stack::Judged::Function(self.closure_function(index)),
                    stack::Report::Own { head },
                )
            }
            FnSource::Named(fn_ref) => (
                stack::Judged::Function(self.named_function(&fn_ref)),
                stack::Report::Fit(at),
            ),
            FnSource::Param(position) => {
                let bound = match &self.abi.params[position].0.ty() {
                    Ty::Fn(fn_ty) => fn_ty.limits.stack,
                    _ => None,
                };
                (stack::Judged::Value(bound), stack::Report::Fit(at))
            }
            FnSource::Other(bound) => (stack::Judged::Value(bound), stack::Report::Fit(at)),
        };
        self.lo.demands.push(stack::Demand {
            judged,
            stack,
            report,
        });
    }

    /// The lowered function of a closure of the unit.
    fn closure_function(&mut self, index: u32) -> FuncId {
        self.lo.instance(Instance::Nir {
            item: self.item,
            role: Role::Closure,
            index,
            args: self.args.clone(),
        })
    }

    /// The lowered function a value naming a function calls.
    fn named_function(&mut self, fn_ref: &FnRef) -> FuncId {
        let fn_ref = FnRef {
            callable: fn_ref.callable.clone(),
            args: self.subst.args(&fn_ref.args),
        };
        let instance = self.lo.callable_instance(&fn_ref, true);
        self.lo.instance(Instance::Value(Box::new(instance)))
    }

    /// The registers a failing call passes to a target block's parameter.
    fn target_args(&mut self, target: BlockId, held: Held, nty: &NTy) -> Vec<IrValue> {
        let param = self.function().blocks[target.0 as usize].params[0];
        let wanted = self.value_repr(param);
        self.adapt(held, nty, wanted).values()
    }
}

impl Lowering<'_, '_> {
    /// The instance a function reference calls: a NIR function, a supplied std function, a
    /// derived method or a supplied conformance, a method through a bound resolved to its
    /// implementation.
    pub fn callable_instance(&mut self, fn_ref: &FnRef, _as_value: bool) -> Instance {
        match &fn_ref.callable {
            Callable::Item { item, role, index } => {
                let has_function = self
                    .ck
                    .unit(*item)
                    .is_some_and(|unit| unit.function(*role, *index).is_some());
                if has_function {
                    return Instance::Nir {
                        item: *item,
                        role: *role,
                        index: *index,
                        args: fn_ref.args.clone(),
                    };
                }
                if *role == Role::Entry {
                    return self.callable_instance(
                        &FnRef {
                            callable: Callable::Item {
                                item: *item,
                                role: Role::Body,
                                index: 0,
                            },
                            args: fn_ref.args.clone(),
                        },
                        false,
                    );
                }
                assert!(
                    self.ck.db.is_std(*item),
                    "a call of {} without a body",
                    self.ck.db.canonical(*item)
                );
                Instance::Supplied {
                    item: *item,
                    args: fn_ref.args.clone(),
                }
            }
            Callable::Derived { owner, method } => Instance::Derived {
                owner: *owner,
                method,
                args: fn_ref.args.clone(),
            },
            Callable::Method { interface, method } => {
                let self_ty = fn_ref
                    .args
                    .first()
                    .and_then(Arg::ty)
                    .cloned()
                    .expect("a Self type");
                let name = self.method_name(*interface, *method);
                match self.ck.implementation(&self_ty, *interface, &name) {
                    Some(implementation)
                        if !matches!(implementation.callable, Callable::Method { .. }) =>
                    {
                        let implementation = self.with_own_args(implementation, &fn_ref.args[1..]);
                        self.callable_instance(&implementation, false)
                    }
                    _ => Instance::Conformance {
                        ty: self_ty,
                        method: name,
                        args: fn_ref.args[1..].iter().cloned().collect(),
                    },
                }
            }
            Callable::Builtin(builtin) => panic!("a function value of the builtin {builtin:?}"),
        }
    }

    /// An implementing function's reference with its own generic parameters given the type
    /// arguments a call through a bound gave the method's.
    fn with_own_args(&self, implementation: FnRef, own: &[Arg]) -> FnRef {
        let Callable::Item { item, .. } = implementation.callable else {
            return implementation;
        };
        let mut own = own.iter();
        let args = self
            .ck
            .sig(item)
            .generics
            .iter()
            .zip(implementation.args.iter())
            .map(|(def, arg)| match def.param.owner == item {
                true => own.next().cloned().unwrap_or_else(|| arg.clone()),
                false => arg.clone(),
            })
            .collect();
        FnRef {
            args,
            ..implementation
        }
    }

    pub fn method_name(&self, interface: ItemId, method: u32) -> String {
        match &self.ck.sig(interface).kind {
            SigKind::Interface { methods } => methods[method as usize].name.clone(),
            _ => panic!("a method of no interface"),
        }
    }

    /// Whether calls of a callable return as a projection does.
    pub fn callable_is_proj(&self, callable: &Callable) -> bool {
        match callable {
            Callable::Item { item, role, index } => match role {
                Role::Body | Role::Entry => self.ck.fn_sig(*item).is_some_and(|sig| sig.is_proj()),
                Role::Default => self.interface_method_is_proj(*item, *index),
                _ => false,
            },
            Callable::Method { interface, method } => self.method_is_proj(*interface, *method),
            _ => false,
        }
    }

    pub fn method_is_proj(&self, interface: ItemId, method: u32) -> bool {
        match &self.ck.sig(interface).kind {
            SigKind::Interface { methods } => methods[method as usize].sig.is_proj(),
            _ => false,
        }
    }

    /// Whether the default body of an interface's `index`th defaulted method is a projection.
    pub fn interface_method_is_proj(&self, interface: ItemId, index: u32) -> bool {
        match &self.ck.sig(interface).kind {
            SigKind::Interface { methods } => methods
                .iter()
                .filter(|method| method.default)
                .nth(index as usize)
                .is_some_and(|method| method.sig.is_proj()),
            _ => false,
        }
    }

    /// The head of an instance as a function type, its types concrete.
    pub fn instance_fn_ty(&mut self, instance: &Instance) -> FnTy {
        match instance {
            Instance::Nir {
                item,
                role,
                index,
                args,
            } => {
                let subst = self.subst(*item, args);
                match role {
                    Role::Default => {
                        let (fn_ty, _) = self.default_head(*item, *index, args);
                        fn_ty
                    }
                    _ => {
                        let sig = self.ck.fn_sig(*item).expect("a function's head");
                        subst.fn_ty(&sig.fn_ty())
                    }
                }
            }
            Instance::Supplied { item, args } => {
                let sig = self.ck.fn_sig(*item).expect("a function's head");
                Subst::of(sig.generics.iter().map(|def| def.param), args).fn_ty(&sig.fn_ty())
            }
            Instance::Derived {
                owner,
                method,
                args,
            } => self.derived_head(*owner, method, args).0,
            Instance::Conformance { ty, method, args } => self.conformance_head(ty, method, args),
            other => panic!("the head of {other:?}"),
        }
    }

    /// The head of an interface method with `Self` and the interface's arguments given, and
    /// whether it is a projection.
    pub fn method_head(
        &mut self,
        self_ty: &Ty,
        interface: ItemId,
        args: &Args,
        method: u32,
    ) -> (FnTy, bool) {
        let sig = self.ck.sig(interface);
        let SigKind::Interface { methods } = &sig.kind else {
            panic!("a method of no interface");
        };
        let method = &methods[method as usize];
        let mut subst = Subst::of(sig.generics.iter().map(|def| def.param), args);
        subst.insert(
            crate::check::ty::Param {
                owner: interface,
                index: crate::check::ty::Param::SELF,
            },
            Arg::Ty(self_ty.clone()),
        );
        (subst.fn_ty(&method.sig.fn_ty()), method.sig.is_proj())
    }

    fn default_head(&mut self, interface: ItemId, index: u32, args: &Args) -> (FnTy, bool) {
        let sig = self.ck.sig(interface);
        let SigKind::Interface { methods } = &sig.kind else {
            panic!("a default body of no interface");
        };
        let position = methods
            .iter()
            .enumerate()
            .filter(|(_, method)| method.default)
            .nth(index as usize)
            .map(|(position, _)| position)
            .expect("a defaulted method");
        let self_ty = args
            .first()
            .and_then(Arg::ty)
            .cloned()
            .expect("a Self type");
        let rest: Args = args[1..].iter().cloned().collect();
        self.method_head(&self_ty, interface, &rest, position as u32)
    }

    /// The calling form of a `Dyn` method: the value's address, then the method's parameters.
    pub fn dyn_abi(&mut self, interface: ItemId, args: &Args, method: u32) -> Abi {
        let placeholder = Ty::Unit;
        let (fn_ty, projection) = self.method_head(&placeholder, interface, args, method);
        let mut params = vec![(NTy::Ref(Ty::Unit), Repr::Address)];
        for param in &fn_ty.params[1..] {
            let nty = self.param_nty(param.mode, param.ty.clone());
            let repr = self.repr(&nty);
            params.push((nty, repr));
        }
        let (result, returned) = self.result_form(&fn_ty.result, projection);
        let fails = super::abi::failure(&fn_ty.fails);
        Abi {
            env: false,
            params,
            result,
            returned,
            flagged: fails.is_some(),
            fails,
        }
    }

    /// The interface arguments of a boxed `Dyn` type.
    pub fn box_dyn_args(&self, ty: &Ty) -> Args {
        match ty.type_arg(0) {
            Some(Ty::Dyn(bound)) => bound.args.clone(),
            _ => panic!("a Dyn call on {ty:?}"),
        }
    }

    /// The external function of a foreign fn and the registers of its parameters and result.
    pub fn foreign(&mut self, item: ItemId) -> (FuncId, Vec<Repr>, Returned) {
        let sig = self.ck.fn_sig(item).expect("a foreign fn's head");
        let params: Vec<Repr> = sig
            .params
            .iter()
            .map(|param| self.owned_repr(&param.ty))
            .collect();
        let returned = match self.owned_repr(&sig.result) {
            Repr::Scalar(ty) => Returned::Scalar(ty),
            Repr::Empty => Returned::Empty,
            Repr::Address => panic!("a foreign fn returning a value in memory"),
        };
        let mut signature = Signature::new(super::CALL_CONV);
        for repr in &params {
            if let Repr::Scalar(ty) = repr {
                signature.params.push(AbiParam::new(*ty));
            }
        }
        if let Returned::Scalar(ty) = returned {
            signature.returns.push(AbiParam::new(ty));
        }
        let name = self.ck.db.declaration(item).name().unwrap_or_default();
        let id = self
            .module
            .declare_function(&name, Linkage::Import, &signature)
            .expect("a foreign symbol");
        (id, params, returned)
    }
}

/// What a call from code lowering writes itself calls.
pub enum Invoked {
    Direct(FuncId),
    /// A function value at an address, with the `stack N` its type demands.
    Value(IrValue, Option<u64>),
}

/// What such a call gives: whether it failed, when it may fail, the address of its failure, and
/// its result.
pub struct Called {
    pub failed: Option<IrValue>,
    pub failure: Option<IrValue>,
    pub result: Held,
}

impl Lowering<'_, '_> {
    /// Calls a function of a calling form with values held as its parameters take them.
    pub fn invoke(&mut self, fx: &mut Fx, invoked: Invoked, abi: &Abi, args: &[Held]) -> Called {
        let mut values = Vec::new();
        if let Invoked::Value(function, _) = invoked {
            values.push(fx.load(PTR, function, 8));
        }
        let sret = (abi.returned == Returned::Indirect).then(|| {
            let layout = self.layout(&abi.result);
            let slot = fx.slot(layout.size, layout.align);
            values.push(slot);
            slot
        });
        let failure = abi.flagged.then(|| {
            let slot = self.failure_slot(fx, abi);
            values.push(slot);
            slot
        });
        for ((nty, repr), held) in abi.params.iter().zip(args) {
            values.extend(adapt_value(self, fx, *held, nty, *repr).values());
        }
        let inst = match invoked {
            Invoked::Direct(id) => {
                let reference = self.func_ref(fx, id);
                fx.b.ins().call(reference, &values)
            }
            Invoked::Value(function, bound) => {
                let code = fx.load(PTR, function, 0);
                let signature = fx.b.import_signature(abi.signature());
                let inst = fx.b.ins().call_indirect(signature, code, &values);
                let kind = match fx.fn_params.get(&function) {
                    Some(position) => stack::Kind::Param(*position),
                    None => stack::Kind::Bounded(bound),
                };
                fx.sites.push((inst, stack::Site { kind, at: None }));
                inst
            }
        };
        let mut results = fx.b.inst_results(inst).to_vec().into_iter();
        let failed = abi.flagged.then(|| results.next().expect("a failure flag"));
        let result = match abi.returned {
            Returned::Scalar(_) => Held::Scalar(results.next().expect("a result")),
            Returned::Part => Held::Address(results.next().expect("a part")),
            Returned::Indirect => Held::Address(sret.expect("a result slot")),
            Returned::Empty => Held::Empty,
        };
        Called {
            failed,
            failure,
            result,
        }
    }

    /// A slot for the failure a call of a calling form writes, of its failure type, or a word
    /// for a function value that cannot fail.
    pub fn failure_slot(&mut self, fx: &mut Fx, abi: &Abi) -> IrValue {
        match &abi.fails {
            Some(fails) => {
                let layout = self.layout(fails);
                fx.slot(layout.size, layout.align)
            }
            None => fx.slot(8, 8),
        }
    }

    /// Calls the function value at an address, of a function type.
    pub fn call_value(
        &mut self,
        fx: &mut Fx,
        fn_ty: &FnTy,
        function: IrValue,
        args: &[Held],
    ) -> Called {
        let abi = self.fn_abi(fn_ty);
        self.invoke(fx, Invoked::Value(function, fn_ty.limits.stack), &abi, args)
    }
}

/// The code of a function value naming `inner`: receives the environment and the parameters as
/// function values pass them, and calls `inner` as it takes them.
pub fn value_glue(lo: &mut Lowering, fx: &mut Fx, abi: &Abi, inner: &Instance) {
    let inner_abi = lo.abi(inner);
    let id = lo.instance(inner.clone());
    forward(lo, fx, abi, &inner_abi, id, true);
}

/// The code a `Dyn` value calls for a method: receives the value's address first and calls the
/// implementing function.
pub fn dyn_glue(
    lo: &mut Lowering,
    fx: &mut Fx,
    abi: &Abi,
    ty: &Ty,
    interface: ItemId,
    args: &Args,
    method: u32,
) {
    let mut fn_args: Vec<Arg> = vec![Arg::Ty(ty.clone())];
    fn_args.extend(args.iter().cloned());
    let fn_ref = FnRef {
        callable: Callable::Method { interface, method },
        args: fn_args.into(),
    };
    let inner = lo.callable_instance(&fn_ref, false);
    let inner_abi = lo.abi(&inner);
    let id = lo.instance(inner);
    let mut outer = abi.clone();
    outer.params[0] = (NTy::Ref(ty.clone()), Repr::Address);
    forward(lo, fx, &outer, &inner_abi, id, false);
}

/// Calls `callee` with the incoming parameters adapted from `outer`'s form to `inner`'s, and
/// returns what it returns as `outer` returns; a function that cannot fail may stand for a
/// function type that may.
fn forward(lo: &mut Lowering, fx: &mut Fx, outer: &Abi, inner: &Abi, callee: FuncId, env: bool) {
    let params = fx.entry();
    let mut incoming = params.into_iter();
    if env {
        incoming.next();
    }
    let mut args = Vec::new();
    if outer.returned == Returned::Indirect {
        args.push(incoming.next().expect("a result address"));
    }
    let failure = outer
        .flagged
        .then(|| incoming.next().expect("a failure address"));
    if inner.flagged {
        args.push(failure.expect("a failure address for a failing function"));
    }
    for ((outer_nty, outer_repr), (_, inner_repr)) in outer.params.iter().zip(&inner.params) {
        let held = match outer_repr {
            Repr::Scalar(_) => Held::Scalar(incoming.next().expect("a parameter")),
            Repr::Address => Held::Address(incoming.next().expect("a parameter")),
            Repr::Empty => Held::Empty,
        };
        let held = adapt_value(lo, fx, held, outer_nty, *inner_repr);
        args.extend(held.values());
    }
    let reference = lo.func_ref(fx, callee);
    let inst = fx.b.ins().call(reference, &args);
    let mut returns = fx.b.inst_results(inst).to_vec();
    if outer.flagged && !inner.flagged {
        returns.insert(0, fx.int(types::I8, 0));
    }
    fx.b.ins().return_(&returns);
}

/// A value held one way, held as `wanted`.
pub fn adapt_value(lo: &mut Lowering, fx: &mut Fx, held: Held, nty: &NTy, wanted: Repr) -> Held {
    match (held, wanted) {
        (Held::Scalar(_), Repr::Address) => {
            let layout = lo.layout(nty.ty());
            let slot = fx.slot(layout.size, layout.align);
            lo.store_held(fx, slot, held, nty.ty());
            Held::Address(slot)
        }
        (Held::Address(address), Repr::Scalar(_)) => lo.held_at(fx, nty.ty(), address),
        (_, Repr::Empty) => Held::Empty,
        (held, _) => held,
    }
}
