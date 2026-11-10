//! Nested calls: the decoding of a type that holds itself, and the writing of the canonical
//! encoding of a value so decoded, run through the `Nested` support routine, on the running
//! task's stack when it has the room the call needs and on a segment otherwise. The call's need,
//! which the nestings a decoding allows bound, is a word of a table the lowering fills once
//! every frame is known.

use std::collections::HashSet;

use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature, Value, types};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};

use super::fx::{Fx, PTR};
use super::runtime::Routine;
use super::stack::{self, Kind, Site};
use super::{CALL_CONV, Glue, Lowering};

/// The symbol of the table of nested calls' needs.
const NEEDS: &str = "_NSstack_nested_needs";

/// The offsets of the record a nested call's thunk reads: the called function's code and its
/// three arguments, the second its allocator, then the code it gives.
pub mod record {
    pub const FUNCTION: u64 = 0;
    pub const ARGS: [u64; 3] = [8, 16, 24];
    pub const CODE: u64 = 32;
    pub const SIZE: u64 = 40;
}

/// What lowering keeps for nested calls: the function each calls, by the index of its need,
/// and the table of needs once declared.
#[derive(Default)]
pub struct Nested {
    called: Vec<FuncId>,
    needs: Option<DataId>,
}

impl Lowering<'_, '_> {
    /// Calls a function of three arguments, the second an allocator, giving a code, through
    /// `Nested`, giving its code.
    pub fn nested_call(&mut self, fx: &mut Fx, function: FuncId, args: &[Value]) -> Value {
        let index = match self
            .nested
            .called
            .iter()
            .position(|known| *known == function)
        {
            Some(index) => index,
            None => {
                self.nested.called.push(function);
                self.nested.called.len() - 1
            }
        };
        let needs = self.nested_needs_data();
        let needs = self.data_address(fx, needs);
        let need = fx.load(PTR, needs, 8 * index as u64);
        let code = self.func_ref(fx, function);
        let code = fx.b.ins().func_addr(PTR, code);
        let routine = self.routine(Routine::Nested);
        let reference = self.func_ref(fx, routine);
        let mut values = vec![code];
        values.extend_from_slice(args);
        values.push(need);
        let call = fx.b.ins().call(reference, &values);
        fx.sites.push((
            call,
            Site {
                kind: Kind::Nested(function),
                at: None,
            },
        ));
        let code = fx.b.inst_results(call)[0];
        fx.b.ins().ireduce(types::I32, code)
    }

    /// The thunk of a nested call: calls the function its record names with the arguments
    /// there, writing its code there.
    pub fn nested_thunk(&mut self) -> FuncId {
        self.glue_function(
            Glue::NestedThunk,
            super::supplied::tasks::record_signature(),
            |_, fx| {
                let held = fx.entry()[0];
                let function = fx.load(PTR, held, record::FUNCTION);
                let args = record::ARGS.map(|offset| fx.load(PTR, held, offset));
                let signature = fx.b.import_signature(nested_signature());
                let call = fx.b.ins().call_indirect(signature, function, &args);
                let code = fx.b.inst_results(call)[0];
                let code = fx.b.ins().uextend(PTR, code);
                fx.store(code, held, record::CODE);
                fx.b.ins().return_(&[]);
            },
        )
    }

    /// The table of nested calls' needs, declared once and defined when lowering finishes.
    fn nested_needs_data(&mut self) -> DataId {
        if let Some(id) = self.nested.needs {
            return id;
        }
        let id = self
            .module
            .declare_data(NEEDS, Linkage::Local, false, false)
            .expect("the needs of nested calls");
        self.nested.needs = Some(id);
        id
    }

    /// Defines the table of nested calls' needs from the frames lowered, zero for a call whose
    /// need is unbounded, which runs on the stack it is made on; and makes each nested call of a
    /// bounded need a call of a support routine.
    pub fn emit_nested_needs(&mut self) {
        let Some(id) = self.nested.needs else {
            return;
        };
        let nesting = self.nesting_functions();
        let needs: Vec<Option<u64>> = (self.nested.called.iter())
            .map(|function| stack::nested_need(&self.frames, *function, &nesting))
            .collect();
        let bounded: HashSet<FuncId> = (self.nested.called.iter())
            .zip(&needs)
            .filter(|(_, need)| need.is_some())
            .map(|(function, _)| *function)
            .collect();
        for site in self
            .frames
            .iter_mut()
            .flat_map(|frame| frame.sites.iter_mut())
        {
            if let Kind::Nested(function) = site.kind
                && bounded.contains(&function)
            {
                site.kind = Kind::Stub;
            }
        }
        let bytes: Vec<u8> = (needs.into_iter())
            .flat_map(|need| need.unwrap_or(0).to_le_bytes())
            .collect();
        let mut description = DataDescription::new();
        description.define(bytes.into_boxed_slice());
        description.set_align(8);
        self.module
            .define_data(id, &description)
            .expect("the needs of nested calls");
    }

    /// The functions lowered whose calls of each other pass a nesting on every cycle: the
    /// decoding functions of the formats bounding their nestings, and the wire format's encoding
    /// functions, which a nested call runs only on a value such a decoding read.
    fn nesting_functions(&self) -> HashSet<FuncId> {
        (self.glues.iter())
            .filter_map(|(glue, id)| match glue {
                Glue::Decode(format, _) if super::supplied::codecs::nests(self, format) => {
                    Some(*id)
                }
                Glue::Encode(super::supplied::codecs::Format::Wire, _) => Some(*id),
                _ => None,
            })
            .collect()
    }
}

/// The signature of a function a nested call calls: three arguments, giving a code.
pub fn nested_signature() -> Signature {
    let mut signature = Signature::new(CALL_CONV);
    for _ in 0..3 {
        signature.params.push(AbiParam::new(PTR));
    }
    signature.returns.push(AbiParam::new(types::I32));
    signature
}
