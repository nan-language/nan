//! The float operations the x86-64 base set with SSE2 has no instruction for, computed exactly
//! in integer arithmetic over the operands' bit patterns: the remainder of a division, and the
//! fused multiply-add, rounded once. Each is a function per float width that lowered code calls.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{AbiParam, InstBuilder, MemFlagsData, Signature, Type, Value, types};
use cranelift_module::FuncId;

use super::super::fx::Fx;
use super::super::{CALL_CONV, Glue, Lowering};

/// A float width: the bits of its fraction and exponent fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Width {
    Single,
    Double,
}

impl Width {
    pub fn of(ty: Type) -> Width {
        match ty {
            types::F32 => Width::Single,
            _ => Width::Double,
        }
    }

    fn float(self) -> Type {
        match self {
            Width::Single => types::F32,
            Width::Double => types::F64,
        }
    }

    fn int(self) -> Type {
        match self {
            Width::Single => types::I32,
            Width::Double => types::I64,
        }
    }

    fn fraction(self) -> i64 {
        match self {
            Width::Single => 23,
            Width::Double => 52,
        }
    }

    fn bias(self) -> i64 {
        match self {
            Width::Single => 127,
            Width::Double => 1_023,
        }
    }

    fn sign_bit(self) -> i64 {
        self.int().bits() as i64 - 1
    }

    /// The bits of a pattern below its sign bit.
    fn magnitude_mask(self) -> i64 {
        ((1u64 << self.sign_bit()) - 1) as i64
    }

    /// The exponent field's value of infinities and NaNs.
    fn top(self) -> i64 {
        self.bias() * 2 + 1
    }

    fn canonical_nan(self) -> i64 {
        match self {
            Width::Single => 0x7FC0_0000,
            Width::Double => 0x7FF8_0000_0000_0000,
        }
    }
}

/// Calls the remainder function of a width on two floats.
pub fn remainder(lo: &mut Lowering, fx: &mut Fx, a: Value, b: Value) -> Value {
    let width = Width::of(fx.b.func.dfg.value_type(a));
    let function = remainder_function(lo, width);
    let reference = lo.func_ref(fx, function);
    let call = fx.b.ins().call(reference, &[a, b]);
    fx.b.inst_results(call)[0]
}

/// Calls the fused multiply-add function of a width on three floats.
pub fn fused_mul_add(lo: &mut Lowering, fx: &mut Fx, a: Value, b: Value, c: Value) -> Value {
    let width = Width::of(fx.b.func.dfg.value_type(a));
    let function = fused_function(lo, width);
    let reference = lo.func_ref(fx, function);
    let call = fx.b.ins().call(reference, &[a, b, c]);
    fx.b.inst_results(call)[0]
}

fn signature(width: Width, count: usize) -> Signature {
    let mut signature = Signature::new(CALL_CONV);
    for _ in 0..count {
        signature.params.push(AbiParam::new(width.float()));
    }
    signature.returns.push(AbiParam::new(width.float()));
    signature
}

/// Returns a bit pattern as a float of a width.
fn return_bits(fx: &mut Fx, width: Width, bits: Value) {
    let float = fx.b.ins().bitcast(width.float(), MemFlagsData::new(), bits);
    fx.b.ins().return_(&[float]);
}

/// `a % b`: a − b × n for the quotient rounded toward zero, by long division of the
/// significands, the result having the sign of `a`.
fn remainder_function(lo: &mut Lowering, width: Width) -> FuncId {
    lo.glue_function(
        Glue::FloatRemainder(width),
        signature(width, 2),
        move |_, fx| {
            let params = fx.entry();
            let int = width.int();
            let fraction = width.fraction();
            let ux = fx.b.ins().bitcast(int, MemFlagsData::new(), params[0]);
            let uy = fx.b.ins().bitcast(int, MemFlagsData::new(), params[1]);
            let ux = widen(fx, width, ux);
            let uy = widen(fx, width, uy);
            let field = |fx: &mut Fx, bits: Value| {
                let shifted = fx.b.ins().ushr_imm_s(bits, fraction);
                fx.b.ins().band_imm_s(shifted, width.top())
            };
            let ex = field(fx, ux);
            let ey = field(fx, uy);
            let sign = fx.b.ins().ushr_imm_s(ux, width.sign_bit());
            let sign = fx.b.ins().ishl_imm_s(sign, width.sign_bit());
            let ax = fx.b.ins().band_imm_s(ux, width.magnitude_mask());
            let ay = fx.b.ins().band_imm_s(uy, width.magnitude_mask());
            let y_zero = fx.b.ins().icmp_imm_s(IntCC::Equal, ay, 0);
            let infinity = width.top() << fraction;
            let y_nan =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThan, ay, infinity);
            let x_special = fx.b.ins().icmp_imm_s(IntCC::Equal, ex, width.top());
            let nan = fx.b.ins().bor(y_zero, y_nan);
            let nan = fx.b.ins().bor(nan, x_special);
            fx.when(nan, |fx| {
                let bits = fx.int(int, width.canonical_nan());
                return_bits(fx, width, bits);
            });
            let not_larger = fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, ax, ay);
            fx.when(not_larger, |fx| {
                let equal = fx.b.ins().icmp(IntCC::Equal, ax, ay);
                fx.when(equal, |fx| {
                    let zero = narrow(fx, width, sign);
                    return_bits(fx, width, zero);
                });
                fx.b.ins().return_(&[params[0]]);
            });
            let (mx, ex) = significand(fx, width, ax, ex);
            let (my, ey) = significand(fx, width, ay, ey);
            let signed_zero = |fx: &mut Fx| {
                let zero = narrow(fx, width, sign);
                return_bits(fx, width, zero);
            };
            let reduced = fx.repeat(&[mx, ex], |fx, state| {
                let (mx, ex) = (state[0], state[1]);
                let above = fx.b.ins().icmp(IntCC::SignedGreaterThan, ex, ey);
                let next = fx.choose(
                    above,
                    &[types::I64, types::I64],
                    |fx| {
                        let fits = fx.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, mx, my);
                        let less = fx.b.ins().isub(mx, my);
                        let exact = fx.b.ins().icmp_imm_s(IntCC::Equal, less, 0);
                        let done = fx.b.ins().band(fits, exact);
                        fx.when(done, |fx| signed_zero(fx));
                        let kept = fx.b.ins().select(fits, less, mx);
                        let kept = fx.b.ins().ishl_imm_s(kept, 1);
                        vec![kept, fx.b.ins().iadd_imm_s(ex, -1)]
                    },
                    |_| vec![mx, ex],
                );
                (above, next)
            });
            let (mx, ex) = (reduced[0], reduced[1]);
            let fits = fx.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, mx, my);
            let less = fx.b.ins().isub(mx, my);
            let exact = fx.b.ins().icmp_imm_s(IntCC::Equal, less, 0);
            let done = fx.b.ins().band(fits, exact);
            fx.when(done, |fx| signed_zero(fx));
            let mx = fx.b.ins().select(fits, less, mx);
            let normal = fx.repeat(&[mx, ex], |fx, state| {
                let (mx, ex) = (state[0], state[1]);
                let lead = fx.b.ins().ushr_imm_s(mx, fraction);
                let short = fx.b.ins().icmp_imm_s(IntCC::Equal, lead, 0);
                let next = fx.choose(
                    short,
                    &[types::I64, types::I64],
                    |fx| {
                        let doubled = fx.b.ins().ishl_imm_s(mx, 1);
                        vec![doubled, fx.b.ins().iadd_imm_s(ex, -1)]
                    },
                    |_| vec![mx, ex],
                );
                (short, next)
            });
            let (mx, ex) = (normal[0], normal[1]);
            let positive = fx.b.ins().icmp_imm_s(IntCC::SignedGreaterThan, ex, 0);
            let bits = fx.choose(
                positive,
                &[types::I64],
                |fx| {
                    let without_lead = fx.b.ins().band_imm_s(mx, (1i64 << fraction) - 1);
                    let exponent = fx.b.ins().ishl_imm_s(ex, fraction);
                    vec![fx.b.ins().bor(without_lead, exponent)]
                },
                |fx| {
                    let shift = subtract_from(fx, 1, ex);
                    vec![fx.b.ins().ushr(mx, shift)]
                },
            )[0];
            let bits = fx.b.ins().bor(bits, sign);
            let bits = narrow(fx, width, bits);
            return_bits(fx, width, bits);
        },
    )
}

/// `constant - value` in 64 bits.
fn subtract_from(fx: &mut Fx, constant: i64, value: Value) -> Value {
    let constant = fx.int(types::I64, constant);
    fx.b.ins().isub(constant, value)
}

/// A width's bit pattern in 64 bits.
fn widen(fx: &mut Fx, width: Width, bits: Value) -> Value {
    match width {
        Width::Single => fx.b.ins().uextend(types::I64, bits),
        Width::Double => bits,
    }
}

/// A 64-bit pattern as a width's.
fn narrow(fx: &mut Fx, width: Width, bits: Value) -> Value {
    match width {
        Width::Single => fx.b.ins().ireduce(types::I32, bits),
        Width::Double => bits,
    }
}

/// The significand of a nonzero finite magnitude with its leading bit at the fraction's width,
/// and the exponent field that goes with it, below 1 for a subnormal.
fn significand(fx: &mut Fx, width: Width, magnitude: Value, field: Value) -> (Value, Value) {
    let fraction = width.fraction();
    let subnormal = fx.b.ins().icmp_imm_s(IntCC::Equal, field, 0);
    let parts = fx.choose(
        subnormal,
        &[types::I64, types::I64],
        |fx| {
            let zeros = fx.b.ins().clz(magnitude);
            let shift = fx.b.ins().iadd_imm_s(zeros, fraction - 63);
            let shifted = fx.b.ins().ishl(magnitude, shift);
            vec![shifted, subtract_from(fx, 1, shift)]
        },
        |fx| {
            let bits = fx.b.ins().band_imm_s(magnitude, (1i64 << fraction) - 1);
            vec![fx.b.ins().bor_imm_s(bits, 1i64 << fraction), field]
        },
    );
    (parts[0], parts[1])
}

/// a × b + c rounded once: the exact product as a 128-bit integer, the addend aligned to it
/// with the bits shifted out kept as a sticky bit, their sum normalized to 63 bits and a sticky
/// bit, then rounded to the width. A single's operands are computed as doubles, which hold
/// them exactly.
fn fused_function(lo: &mut Lowering, width: Width) -> FuncId {
    lo.glue_function(
        Glue::FusedMulAdd(width),
        signature(width, 3),
        move |_, fx| {
            let params = fx.entry();
            let (a, b, c) = (params[0], params[1], params[2]);
            let special = |fx: &mut Fx, value: Value| {
                let bits = fx.b.ins().bitcast(width.int(), MemFlagsData::new(), value);
                let bits = widen(fx, width, bits);
                let magnitude = fx.b.ins().band_imm_s(bits, width.magnitude_mask());
                let zero = fx.b.ins().icmp_imm_s(IntCC::Equal, magnitude, 0);
                let infinity = width.top() << width.fraction();
                let top =
                    fx.b.ins()
                        .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, magnitude, infinity);
                (fx.b.ins().bor(zero, top), zero)
            };
            let (a_special, _) = special(fx, a);
            let (b_special, _) = special(fx, b);
            let product_special = fx.b.ins().bor(a_special, b_special);
            fx.when(product_special, |fx| {
                let product = fx.b.ins().fmul(a, b);
                let sum = fx.b.ins().fadd(product, c);
                fx.b.ins().return_(&[sum]);
            });
            let (c_special, c_zero) = special(fx, c);
            let c_top = fx.b.ins().bxor(c_special, c_zero);
            fx.when(c_top, |fx| {
                let zero = match width {
                    Width::Single => fx.b.ins().f32const(0.0),
                    Width::Double => fx.b.ins().f64const(0.0),
                };
                let kept = fx.b.ins().fadd(c, zero);
                fx.b.ins().return_(&[kept]);
            });
            let double = |fx: &mut Fx, value: Value| match width {
                Width::Single => fx.b.ins().fpromote(types::F64, value),
                Width::Double => value,
            };
            let (x, y, z) = (double(fx, a), double(fx, b), double(fx, c));
            let (mx, ex, sx) = normalized(fx, x);
            let (my, ey, sy) = normalized(fx, y);
            let low = fx.b.ins().imul(mx, my);
            let high = fx.b.ins().umulhi(mx, my);
            let e = fx.b.ins().iadd(ex, ey);
            let sign = fx.b.ins().bxor(sx, sy);
            let zero = fx.ptr(0);
            let aligned = fx.choose(
                c_zero,
                &[types::I64; 6],
                |_| vec![high, low, zero, zero, e, sign],
                |fx| {
                    let (mz, ez, sz) = normalized(fx, z);
                    let (high, low, zhigh, zlow, e) = align(fx, high, low, e, mz, ez);
                    let same = fx.b.ins().icmp(IntCC::Equal, sign, sz);
                    let summed = fx.choose(
                        same,
                        &[types::I64; 3],
                        |fx| {
                            let low_sum = fx.b.ins().iadd(low, zlow);
                            let carry = fx.b.ins().icmp(IntCC::UnsignedLessThan, low_sum, zlow);
                            let carry = fx.b.ins().uextend(types::I64, carry);
                            let high_sum = fx.b.ins().iadd(high, zhigh);
                            vec![fx.b.ins().iadd(high_sum, carry), low_sum, sign]
                        },
                        |fx| {
                            let low_difference = fx.b.ins().isub(low, zlow);
                            let borrow =
                                fx.b.ins()
                                    .icmp(IntCC::UnsignedLessThan, low, low_difference);
                            let borrow = fx.b.ins().uextend(types::I64, borrow);
                            let high_difference = fx.b.ins().isub(high, zhigh);
                            let high_difference = fx.b.ins().isub(high_difference, borrow);
                            let negative =
                                fx.b.ins()
                                    .icmp_imm_s(IntCC::SignedLessThan, high_difference, 0);
                            fx.choose(
                                negative,
                                &[types::I64; 3],
                                |fx| {
                                    let low_negated = fx.b.ins().ineg(low_difference);
                                    let nonzero =
                                        fx.b.ins().icmp_imm_s(IntCC::NotEqual, low_negated, 0);
                                    let nonzero = fx.b.ins().uextend(types::I64, nonzero);
                                    let high_negated = fx.b.ins().ineg(high_difference);
                                    let high_negated = fx.b.ins().isub(high_negated, nonzero);
                                    let flipped = fx.b.ins().bxor_imm_s(sign, 1);
                                    vec![high_negated, low_negated, flipped]
                                },
                                |_| vec![high_difference, low_difference, sign],
                            )
                        },
                    );
                    vec![summed[0], summed[1], zero, zero, e, summed[2]]
                },
            );
            let (high, low, e, sign) = (aligned[0], aligned[1], aligned[4], aligned[5]);
            let empty = fx.b.ins().bor(high, low);
            let empty = fx.b.ins().icmp_imm_s(IntCC::Equal, empty, 0);
            fx.when(empty, |fx| {
                let bits = fx.int(width.int(), 0);
                return_bits(fx, width, bits);
            });
            let (top, e) = normalize_sum(fx, high, low, e);
            let bits = round(fx, width, sign, top, e);
            let bits = narrow(fx, width, bits);
            return_bits(fx, width, bits);
        },
    )
}

/// A nonzero finite double as an integer significand with its top 10 bits and last bit clear,
/// the power of two scaling it and its sign bit.
fn normalized(fx: &mut Fx, value: Value) -> (Value, Value, Value) {
    let bits = fx.b.ins().bitcast(types::I64, MemFlagsData::new(), value);
    let sign = fx.b.ins().ushr_imm_s(bits, 63);
    let magnitude = fx.b.ins().band_imm_s(bits, i64::MAX);
    let field = fx.b.ins().ushr_imm_s(magnitude, 52);
    let (significand, field) = significand(fx, Width::Double, magnitude, field);
    let shifted = fx.b.ins().ishl_imm_s(significand, 1);
    let exponent = fx.b.ins().iadd_imm_s(field, -(1_023 + 52 + 1));
    (shifted, exponent, sign)
}

/// Aligns an addend's significand `mz` scaled by 2^`ez` with a 128-bit product scaled by
/// 2^`e`: shifts the addend left, or the product right with a sticky bit, and returns the
/// product's words, the addend's and their common scale.
fn align(
    fx: &mut Fx,
    high: Value,
    low: Value,
    e: Value,
    mz: Value,
    ez: Value,
) -> (Value, Value, Value, Value, Value) {
    let d = fx.b.ins().isub(ez, e);
    let above = fx.b.ins().icmp_imm_s(IntCC::SignedGreaterThan, d, 0);
    let parts = fx.choose(
        above,
        &[types::I64; 5],
        |fx| {
            let near = fx.b.ins().icmp_imm_s(IntCC::SignedLessThan, d, 64);
            fx.choose(
                near,
                &[types::I64; 5],
                |fx| {
                    let zlow = fx.b.ins().ishl(mz, d);
                    let back = subtract_from(fx, 64, d);
                    let zhigh = fx.b.ins().ushr(mz, back);
                    vec![high, low, zhigh, zlow, e]
                },
                |fx| {
                    let rest = fx.b.ins().iadd_imm_s(d, -64);
                    let scale = fx.b.ins().iadd_imm_s(ez, -64);
                    let (high, low) = shift_right_sticky(fx, high, low, rest);
                    let zero = fx.ptr(0);
                    vec![high, low, mz, zero, scale]
                },
            )
        },
        |fx| {
            let distance = fx.b.ins().ineg(d);
            let zlow = shift_sticky(fx, mz, distance);
            let zero = fx.ptr(0);
            vec![high, low, zero, zlow, e]
        },
    );
    (parts[0], parts[1], parts[2], parts[3], parts[4])
}

/// A 128-bit value shifted right by `distance` bits, at least 0, keeping whether a set bit was
/// shifted out in its last bit.
fn shift_right_sticky(fx: &mut Fx, high: Value, low: Value, distance: Value) -> (Value, Value) {
    let zero = fx.b.ins().icmp_imm_s(IntCC::Equal, distance, 0);
    let near = fx.b.ins().icmp_imm_s(IntCC::SignedLessThan, distance, 64);
    let parts = fx.choose(
        zero,
        &[types::I64, types::I64],
        |_| vec![high, low],
        |fx| {
            fx.choose(
                near,
                &[types::I64, types::I64],
                |fx| {
                    let back = subtract_from(fx, 64, distance);
                    let carried = fx.b.ins().ishl(high, back);
                    let kept = fx.b.ins().ushr(low, distance);
                    let lost = fx.b.ins().ishl(low, back);
                    let lost = fx.b.ins().icmp_imm_s(IntCC::NotEqual, lost, 0);
                    let lost = fx.b.ins().uextend(types::I64, lost);
                    let low = fx.b.ins().bor(carried, kept);
                    let low = fx.b.ins().bor(low, lost);
                    vec![fx.b.ins().ushr(high, distance), low]
                },
                |fx| vec![fx.ptr(0), fx.ptr(1)],
            )
        },
    );
    (parts[0], parts[1])
}

/// A 64-bit value shifted right by `distance` bits, at least 0, keeping whether a set bit was
/// shifted out in its last bit; a nonzero value shifted past every bit is 1.
fn shift_sticky(fx: &mut Fx, value: Value, distance: Value) -> Value {
    let zero = fx.b.ins().icmp_imm_s(IntCC::Equal, distance, 0);
    let near = fx.b.ins().icmp_imm_s(IntCC::SignedLessThan, distance, 64);
    fx.choose(
        zero,
        &[types::I64],
        |_| vec![value],
        |fx| {
            fx.choose(
                near,
                &[types::I64],
                |fx| {
                    let kept = fx.b.ins().ushr(value, distance);
                    let back = subtract_from(fx, 64, distance);
                    let lost = fx.b.ins().ishl(value, back);
                    let lost = fx.b.ins().icmp_imm_s(IntCC::NotEqual, lost, 0);
                    let lost = fx.b.ins().uextend(types::I64, lost);
                    vec![fx.b.ins().bor(kept, lost)]
                },
                |fx| vec![fx.ptr(1)],
            )
        },
    )[0]
}

/// A nonzero 128-bit sum scaled by 2^`e` as its top 63 bits, the last of them sticky, and the
/// power of two scaling them.
fn normalize_sum(fx: &mut Fx, high: Value, low: Value, e: Value) -> (Value, Value) {
    let in_high = fx.b.ins().icmp_imm_s(IntCC::NotEqual, high, 0);
    let parts = fx.choose(
        in_high,
        &[types::I64, types::I64],
        |fx| {
            let zeros = fx.b.ins().clz(high);
            let d = fx.b.ins().iadd_imm_s(zeros, -1);
            let raised = fx.b.ins().ishl(high, d);
            let back = subtract_from(fx, 64, d);
            let carried = fx.b.ins().ushr(low, back);
            let lost = fx.b.ins().ishl(low, d);
            let lost = fx.b.ins().icmp_imm_s(IntCC::NotEqual, lost, 0);
            let lost = fx.b.ins().uextend(types::I64, lost);
            let top = fx.b.ins().bor(raised, carried);
            let top = fx.b.ins().bor(top, lost);
            let e = fx.b.ins().iadd_imm_s(e, 64);
            vec![top, fx.b.ins().isub(e, d)]
        },
        |fx| {
            let zeros = fx.b.ins().clz(low);
            let full = fx.b.ins().icmp_imm_s(IntCC::Equal, zeros, 0);
            fx.choose(
                full,
                &[types::I64, types::I64],
                |fx| {
                    let halved = fx.b.ins().ushr_imm_s(low, 1);
                    let last = fx.b.ins().band_imm_s(low, 1);
                    vec![fx.b.ins().bor(halved, last), fx.b.ins().iadd_imm_s(e, 1)]
                },
                |fx| {
                    let d = fx.b.ins().iadd_imm_s(zeros, -1);
                    vec![fx.b.ins().ishl(low, d), fx.b.ins().isub(e, d)]
                },
            )
        },
    );
    (parts[0], parts[1])
}

/// The bit pattern, in 64 bits, of `top` × 2^`e` rounded to a width by roundTiesToEven, `top`
/// holding 63 bits with its leading bit at 62 and its last bit sticky.
fn round(fx: &mut Fx, width: Width, sign: Value, top: Value, e: Value) -> Value {
    let precision = width.fraction() + 1;
    let lowest = 1 - width.bias();
    let leading = fx.b.ins().iadd_imm_s(e, 62);
    let normal =
        fx.b.ins()
            .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, leading, lowest);
    let below = subtract_from(fx, lowest, leading);
    let zero = fx.ptr(0);
    let extra = fx.b.ins().select(normal, zero, below);
    let shift = fx.b.ins().iadd_imm_s(extra, 63 - precision);
    let gone =
        fx.b.ins()
            .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, shift, 64);
    let rounded = fx.choose(
        gone,
        &[types::I64],
        |fx| vec![fx.ptr(0)],
        |fx| {
            let kept = fx.b.ins().ushr(top, shift);
            let half_at = fx.b.ins().iadd_imm_s(shift, -1);
            let half = fx.b.ins().ushr(top, half_at);
            let half = fx.b.ins().band_imm_s(half, 1);
            let one = fx.ptr(1);
            let below_half = fx.b.ins().ishl(one, half_at);
            let below_half = fx.b.ins().iadd_imm_s(below_half, -1);
            let rest = fx.b.ins().band(top, below_half);
            let sticky = fx.b.ins().icmp_imm_s(IntCC::NotEqual, rest, 0);
            let odd = fx.b.ins().band_imm_s(kept, 1);
            let odd = fx.b.ins().icmp_imm_s(IntCC::NotEqual, odd, 0);
            let up = fx.b.ins().bor(sticky, odd);
            let half = fx.b.ins().icmp_imm_s(IntCC::NotEqual, half, 0);
            let up = fx.b.ins().band(half, up);
            let up = fx.b.ins().uextend(types::I64, up);
            vec![fx.b.ins().iadd(kept, up)]
        },
    )[0];
    let sign = fx.b.ins().ishl_imm_s(sign, width.sign_bit());
    let bits = fx.choose(
        normal,
        &[types::I64],
        |fx| {
            let carried = fx.b.ins().ushr_imm_s(rounded, precision);
            let significand = fx.b.ins().ushr(rounded, carried);
            let exponent = fx.b.ins().iadd(leading, carried);
            let biased = fx.b.ins().iadd_imm_s(exponent, width.bias());
            let overflow =
                fx.b.ins()
                    .icmp_imm_s(IntCC::SignedGreaterThanOrEqual, biased, width.top());
            fx.choose(
                overflow,
                &[types::I64],
                |fx| vec![fx.ptr((width.top() << width.fraction()) as u64)],
                |fx| {
                    let fraction =
                        fx.b.ins()
                            .band_imm_s(significand, (1i64 << width.fraction()) - 1);
                    let field = fx.b.ins().ishl_imm_s(biased, width.fraction());
                    vec![fx.b.ins().bor(field, fraction)]
                },
            )
        },
        |_| vec![rounded],
    )[0];
    fx.b.ins().bor(bits, sign)
}
