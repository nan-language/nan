//! Jobs a kernel runs from the bundles of nanos programs: the call of a loaded bundle's job entry,
//! the rendering of a job's failure into a buffer, and the `host::World` a host-compatible entry
//! receives.

use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature};

use super::super::Lowering;
use super::super::body::Held;
use super::super::fx::{Fx, PTR};
use super::sys::view;
use super::{Supply, render};
use crate::check::ty::Ty;

/// The fields of a `host::World` that `nanos::host_world` receives, in its parameters' order.
const HOST_FIELDS: [&str; 11] = [
    "fs",
    "net",
    "clock",
    "wall",
    "random",
    "log",
    "stdio",
    "alloc",
    "exec",
    "args",
    "environment",
];

pub fn lower(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) -> bool {
    match supply.path.as_str() {
        "nanos::call" => call(lo, fx, supply),
        "nanos::render_into" => {
            let (buffer, len) = view(fx, supply.param(1));
            let renderer = render::new_renderer(fx, buffer, len);
            let ty = supply.type_arg(0);
            let args = [supply.param(0), Held::Address(renderer)];
            lo.call_method(fx, &ty, "Render", Vec::new(), &args);
            let written = render::finish(lo, fx, renderer);
            supply.ret(lo, fx, Held::Scalar(written));
        }
        "nanos::host_world" => {
            let world = supply.abi.result.clone();
            let sret = supply.sret.expect("a world's address");
            for (index, name) in HOST_FIELDS.iter().enumerate() {
                let field = lo.layouts.field(lo.ck, &world, None, name);
                let at = fx.offset(sret, field.offset);
                lo.store_held(fx, at, supply.param(index), &field.ty);
            }
            supply.ret(lo, fx, Held::Address(sret));
        }
        _ => return false,
    }
    true
}

/// `nanos::call(job, world, input)`: calls the job entry's code with the addresses of the world,
/// of the input and of the slot it writes how the job ended into.
fn call(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    let code = fx.load(PTR, supply.param(0).address(), 0);
    let ended: Ty = supply.abi.result.clone();
    let layout = lo.layout(&ended);
    let slot = supply
        .sret
        .unwrap_or_else(|| fx.slot(layout.size, layout.align));
    let mut signature = Signature::new(super::super::CALL_CONV);
    signature.params.extend([AbiParam::new(PTR); 3]);
    let signature = fx.b.import_signature(signature);
    let world = supply.param(1).address();
    let input = supply.param(2).address();
    fx.b.ins()
        .call_indirect(signature, code, &[world, input, slot]);
    let held = lo.held_at(fx, &ended, slot);
    supply.ret(lo, fx, held);
}
