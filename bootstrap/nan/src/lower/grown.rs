//! Grown calls: in an image whose task stacks grow, a direct call closing a cycle of the calls
//! emitted so far, and a call of a function value whose type demands no `stack N`, go through a
//! wrapper. The wrapper writes the call's arguments into a record and has the `Grow` support
//! routine run its thunk, which makes the call from the record, on the running stack when it has
//! the room the thunk needs and on a new stack segment otherwise. Each thunk's need is a word of
//! a table the lowering fills once every frame is known.

use std::collections::{HashMap, HashSet};

use cranelift_codegen::ir::{self, AbiParam, InstBuilder, Signature, Value};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};

use super::fx::{Fx, PTR};
use super::runtime::Routine;
use super::{Glue, Lowering, stack};

/// The symbol of the table of grown calls' needs.
const NEEDS: &str = "_NSstack_switch_needs";

/// The bytes each argument and result takes in a grown call's record.
const SLOT: u64 = 16;

/// What a grown call calls: a lowered function, or a function value's code of a signature.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Called {
    Function(FuncId),
    Value(Signature),
}

/// What lowering keeps for grown calls.
#[derive(Default)]
pub struct Growth {
    /// The functions being defined, innermost last, with whether calls they make may grow.
    defining: Vec<(FuncId, bool)>,
    /// The direct calls emitted from functions whose calls may grow, by caller.
    calls: HashMap<FuncId, Vec<FuncId>>,
    /// Each wrapper's thunk and callee, none for a function value's code.
    wrappers: HashMap<FuncId, (FuncId, Option<FuncId>)>,
    /// Each grown call's thunk and callee, by the index of its need.
    grown: Vec<stack::Grown>,
    needs: Option<DataId>,
}

impl Growth {
    /// Whether `from` reaches `to` through the calls emitted so far.
    fn reaches(&self, from: FuncId, to: FuncId) -> bool {
        let mut seen = HashSet::from([from]);
        let mut pending = vec![from];
        while let Some(caller) = pending.pop() {
            for callee in self.calls.get(&caller).into_iter().flatten() {
                if *callee == to {
                    return true;
                }
                if seen.insert(*callee) {
                    pending.push(*callee);
                }
            }
        }
        false
    }

    /// The call site a call of a wrapper is, given how lowering classified the call it wraps.
    pub fn site(&self, callee: FuncId, recorded: Option<stack::Kind>) -> Option<stack::Kind> {
        let (thunk, wrapped) = self.wrappers.get(&callee)?;
        let inner = recorded.unwrap_or_else(|| match wrapped {
            Some(function) => stack::Kind::Direct {
                callee: *function,
                bindings: Vec::new(),
            },
            None => stack::Kind::Bounded(None),
        });
        Some(stack::Kind::Grown {
            wrapper: callee,
            thunk: *thunk,
            inner: Box::new(inner),
        })
    }
}

impl Lowering<'_, '_> {
    /// Whether the image's runtime keeps stack segments: a kernel exporting the load ABI, whose
    /// tasks run the grown calls of the nanos bundles it loads.
    pub fn keeps_segments(&self) -> bool {
        self.exports()
    }

    /// Starts defining a function: a routine's calls never grow.
    pub fn enter_definition(&mut self, id: FuncId) {
        if let Some(growth) = &mut self.growth {
            let routine = self.routines.values().any(|routine| *routine == id);
            growth.defining.push((id, !routine));
        }
    }

    pub fn leave_definition(&mut self) {
        if let Some(growth) = &mut self.growth {
            growth.defining.pop();
        }
    }

    /// A reference for a direct call of a lowered function from the function being built: its
    /// grown call's wrapper when the callee reaches the caller through the calls emitted so far.
    pub fn call_ref(&mut self, fx: &mut Fx, id: FuncId) -> ir::FuncRef {
        let callee = self.grown_callee(id).unwrap_or(id);
        self.func_ref(fx, callee)
    }

    fn grown_callee(&mut self, callee: FuncId) -> Option<FuncId> {
        let growth = self.growth.as_mut()?;
        let (caller, grows) = *growth.defining.last()?;
        if !grows {
            return None;
        }
        let closes = callee == caller || growth.reaches(callee, caller);
        growth.calls.entry(caller).or_default().push(callee);
        closes.then(|| self.wrapper(Called::Function(callee)))
    }

    /// Calls code through its address, as a call of a function value whose type demands
    /// `stack` bytes or none: an unbounded one as a grown call when the image's stacks grow.
    pub fn call_code(
        &mut self,
        fx: &mut Fx,
        signature: Signature,
        code: Value,
        args: &[Value],
        stack: Option<u64>,
    ) -> ir::Inst {
        let grows = self
            .growth
            .as_ref()
            .and_then(|growth| growth.defining.last())
            .is_some_and(|(_, grows)| *grows && stack.is_none());
        if !grows {
            let signature = fx.b.import_signature(signature);
            return fx.b.ins().call_indirect(signature, code, args);
        }
        let wrapper = self.wrapper(Called::Value(signature));
        let reference = self.func_ref(fx, wrapper);
        let mut values = vec![code];
        values.extend_from_slice(args);
        fx.b.ins().call(reference, &values)
    }

    /// The wrapper of a grown call, defined after its thunk when first asked for.
    fn wrapper(&mut self, called: Called) -> FuncId {
        let glue = Glue::Grown(called.clone());
        if let Some(wrapper) = self.glue(&glue) {
            return wrapper;
        }
        let thunk = self.thunk(&called);
        let (signature, callee) = match &called {
            Called::Function(callee) => {
                let declaration = self.module.declarations().get_function_decl(*callee);
                (declaration.signature.clone(), Some(*callee))
            }
            Called::Value(signature) => {
                let mut signature = signature.clone();
                signature.params.insert(0, AbiParam::new(PTR));
                (signature, None)
            }
        };
        let growth = self.growth.as_mut().expect("an image whose stacks grow");
        growth.grown.push(stack::Grown { thunk, callee });
        let index = growth.grown.len() - 1;
        let wrapper_signature = signature.clone();
        let wrapper = self.glue_function(glue, wrapper_signature, move |lo, fx| {
            let params = fx.entry();
            let record = fx.slot(SLOT * slots(&signature), SLOT);
            for (at, param) in params.iter().enumerate() {
                fx.store(*param, record, SLOT * at as u64);
            }
            let thunk = lo.func_ref(fx, thunk);
            let thunk = fx.b.ins().func_addr(PTR, thunk);
            let needs = lo.needs_data();
            let needs = lo.data_address(fx, needs);
            let need = fx.load(PTR, needs, 8 * index as u64);
            lo.call_routine(fx, Routine::Grow, &[thunk, record, need]);
            let results: Vec<Value> = signature
                .returns
                .iter()
                .enumerate()
                .map(|(at, result)| fx.load(result.value_type, record, SLOT * at as u64))
                .collect();
            fx.b.ins().return_(&results);
        });
        let growth = self.growth.as_mut().expect("an image whose stacks grow");
        growth.wrappers.insert(wrapper, (thunk, callee));
        wrapper
    }

    /// The thunk of a grown call: makes the call from the record of its arguments, writing its
    /// results over them.
    fn thunk(&mut self, called: &Called) -> FuncId {
        let called = called.clone();
        let signature = super::supplied::tasks::record_signature();
        self.glue_function(
            Glue::GrownThunk(called.clone()),
            signature,
            move |lo, fx| {
                let record = fx.entry()[0];
                let inst = match &called {
                    Called::Function(callee) => {
                        let signature = lo
                            .module
                            .declarations()
                            .get_function_decl(*callee)
                            .signature
                            .clone();
                        let args = loaded(fx, &signature, record, 0);
                        let reference = lo.func_ref(fx, *callee);
                        fx.b.ins().call(reference, &args)
                    }
                    Called::Value(signature) => {
                        let code = fx.load(PTR, record, 0);
                        let args = loaded(fx, signature, record, 1);
                        let signature = fx.b.import_signature(signature.clone());
                        fx.b.ins().call_indirect(signature, code, &args)
                    }
                };
                let results = fx.b.inst_results(inst).to_vec();
                for (at, result) in results.into_iter().enumerate() {
                    fx.store(result, record, SLOT * at as u64);
                }
                fx.b.ins().return_(&[]);
            },
        )
    }

    /// The table of grown calls' needs, declared once and defined when lowering finishes.
    fn needs_data(&mut self) -> DataId {
        let growth = self.growth.as_mut().expect("an image whose stacks grow");
        if let Some(id) = growth.needs {
            return id;
        }
        let id = self
            .module
            .declare_data(NEEDS, Linkage::Local, false, false)
            .expect("the needs of grown calls");
        growth.needs = Some(id);
        id
    }

    /// Defines the table of grown calls' needs from the frames lowered.
    pub fn emit_needs(&mut self) {
        let Some(growth) = &self.growth else {
            return;
        };
        let Some(id) = growth.needs else {
            return;
        };
        let mut excluded: HashSet<FuncId> = self.routines.values().copied().collect();
        for (wrapper, (thunk, _)) in &growth.wrappers {
            excluded.extend([*wrapper, *thunk]);
        }
        let needs = stack::needs(&self.frames, &growth.grown, &excluded);
        let bytes: Vec<u8> = needs.iter().flat_map(|need| need.to_le_bytes()).collect();
        let mut description = DataDescription::new();
        description.define(bytes.into_boxed_slice());
        description.set_align(8);
        self.module
            .define_data(id, &description)
            .expect("the needs of grown calls");
    }
}

/// The record slots a call of a signature takes: one per argument or result, whichever are more.
fn slots(signature: &Signature) -> u64 {
    signature.params.len().max(signature.returns.len()).max(1) as u64
}

/// The arguments of a call of a signature, read from a record from slot `first` on.
fn loaded(fx: &mut Fx, signature: &Signature, record: Value, first: u64) -> Vec<Value> {
    signature
        .params
        .iter()
        .enumerate()
        .map(|(at, param)| fx.load(param.value_type, record, SLOT * (first + at as u64)))
        .collect()
}
