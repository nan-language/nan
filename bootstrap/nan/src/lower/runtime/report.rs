//! Trap reports: the trap routine walks the frame pointers from the trapping operation, finds
//! each return address in the line tables, and writes at most 15 lines: the kind and location,
//! the source line, the operands, and one line per caller outside std, innermost first.
//!
//! A report is written into a record on the trap routine's stack: its length, then its bytes.
//! The scalar and text routines, and the glue writing values of other types, append to it.

pub mod values;

use cranelift_codegen::ir::{
    AbiParam, InstBuilder, MemFlagsData, Signature, TrapCode, Value, condcodes::IntCC, types,
};
use cranelift_frontend::Variable;

use super::super::body::Held;
use super::super::fx::{Fx, PTR};
use super::super::lines::{self, ENTRY_RECORD, FUNCTION_RECORD, LOCATION_RECORD};
use super::super::trap::{KINDS, OPERAND_RECORD, Written};
use super::super::{CALL_CONV, Lowering};
use super::tasks::{self, trap_record};
use super::{Routine, state};

/// The most bytes a report holds.
pub const REPORT: u64 = 4_096;

/// The most frames a walk reads.
const FRAMES: u64 = 64;

/// The most lines a report holds.
const LINES: u64 = 15;

pub fn define(lo: &mut Lowering, fx: &mut Fx, routine: Routine) {
    match routine {
        Routine::WriteErr => {
            let params = fx.entry();
            let (text, len) = (params[0], params[1]);
            let two = fx.int(types::I32, 2);
            fx.repeat(&[text, len], |fx, state| {
                let (at, left) = (state[0], state[1]);
                let written = lo.call_libc(fx, "write", &[two, at, left], &[PTR])[0];
                let progressed = fx.b.ins().icmp_imm_s(IntCC::SignedGreaterThan, written, 0);
                let next_at = fx.b.ins().iadd(at, written);
                let next_left = fx.b.ins().isub(left, written);
                let zero = fx.ptr(0);
                let next_left = fx.b.ins().select(progressed, next_left, zero);
                let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, next_left, 0);
                (more, vec![next_at, next_left])
            });
            fx.b.ins().return_(&[]);
        }
        Routine::TrapRaise => raise(lo, fx),
        Routine::ReportScalar => {
            let params = fx.entry();
            Report { out: params[0] }.scalar(fx, lo, params[1], params[2]);
            fx.b.ins().return_(&[]);
        }
        Routine::ReportText => {
            let params = fx.entry();
            let (start, len, bytes) = (params[1], params[2], params[3]);
            Report { out: params[0] }.literal(fx, lo, start, len, bytes);
            fx.b.ins().return_(&[]);
        }
        Routine::OverflowRaise => {
            fx.entry();
            raise_kind(lo, fx, "trap.stack-overflow");
        }
        _ => unreachable!("a report routine"),
    }
}

/// The signature of a glue writing a value in a report: the report and the value's address.
pub fn writer_signature() -> Signature {
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(PTR));
    signature.params.push(AbiParam::new(PTR));
    signature
}

/// A report being written: the address of its record.
#[derive(Clone, Copy)]
pub struct Report {
    pub out: Value,
}

impl Report {
    /// Appends a byte range, unless the report has no room left for all of it.
    pub fn bytes(self, fx: &mut Fx, lo: &mut Lowering, text: Value, len: Value) {
        let at = fx.load(PTR, self.out, 0);
        let room = fx.ptr(REPORT);
        let end = fx.b.ins().iadd(at, len);
        let fits = fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, end, room);
        let out = self.out;
        fx.when(fits, |fx| {
            let bytes = fx.b.ins().iadd_imm_s(out, 8);
            let target = fx.b.ins().iadd(bytes, at);
            lo.call_routine(fx, Routine::Copy, &[target, text, len]);
            fx.store(end, out, 0);
        });
    }

    pub fn text(self, fx: &mut Fx, lo: &mut Lowering, text: &str) {
        let id = lo.bytes_data(text.as_bytes());
        let global = lo.data_ref(fx, id);
        let address = fx.b.ins().symbol_value(PTR, global);
        let len = fx.ptr(text.len() as u64);
        self.bytes(fx, lo, address, len);
    }

    /// Appends one byte, the low byte of `value`.
    fn byte(self, fx: &mut Fx, lo: &mut Lowering, value: Value) {
        let slot = fx.slot(1, 1);
        let byte = match fx.b.func.dfg.value_type(value) {
            types::I8 => value,
            _ => fx.b.ins().ireduce(types::I8, value),
        };
        fx.store(byte, slot, 0);
        let one = fx.ptr(1);
        self.bytes(fx, lo, slot, one);
    }

    /// Whether the report has no room left.
    pub fn full(self, fx: &mut Fx) -> Value {
        let at = fx.load(PTR, self.out, 0);
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, at, REPORT as i64)
    }

    /// Writes an unsigned number in decimal.
    fn decimal(self, fx: &mut Fx, lo: &mut Lowering, value: Value) {
        let digits = fx.slot(24, 8);
        let end = fx.ptr(24);
        let state = fx.repeat(&[value, end], |fx, state| {
            let (rest, at) = (state[0], state[1]);
            let ten = fx.ptr(10);
            let digit = fx.b.ins().urem(rest, ten);
            let rest = fx.b.ins().udiv(rest, ten);
            let at = fx.b.ins().iadd_imm_s(at, -1);
            let ascii = fx.b.ins().iadd_imm_s(digit, i64::from(b'0'));
            let ascii = fx.b.ins().ireduce(types::I8, ascii);
            let place = fx.b.ins().iadd(digits, at);
            fx.store(ascii, place, 0);
            let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, rest, 0);
            (more, vec![rest, at])
        });
        let start = fx.b.ins().iadd(digits, state[1]);
        let len = fx.b.ins().isub(end, state[1]);
        self.bytes(fx, lo, start, len);
    }

    /// Writes a number in uppercase hexadecimal digits: without leading zeros, or with exactly
    /// `width` digits.
    fn hex(self, fx: &mut Fx, lo: &mut Lowering, value: Value, width: Option<u64>) {
        let digits = fx.slot(16, 8);
        let end = fx.ptr(16);
        let state = fx.repeat(&[value, end], |fx, state| {
            let (rest, at) = (state[0], state[1]);
            let nibble = fx.b.ins().band_imm_s(rest, 0xF);
            let letter =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, nibble, 10);
            let digit = fx.b.ins().iadd_imm_s(nibble, i64::from(b'0'));
            let upper = fx.b.ins().iadd_imm_s(nibble, i64::from(b'A') - 10);
            let ascii = fx.b.ins().select(letter, upper, digit);
            let ascii = fx.b.ins().ireduce(types::I8, ascii);
            let at = fx.b.ins().iadd_imm_s(at, -1);
            let place = fx.b.ins().iadd(digits, at);
            fx.store(ascii, place, 0);
            let rest = fx.b.ins().ushr_imm_s(rest, 4);
            let more = match width {
                Some(width) => {
                    fx.b.ins()
                        .icmp_imm_s(IntCC::UnsignedGreaterThan, at, 16 - width as i64)
                }
                None => fx.b.ins().icmp_imm_s(IntCC::NotEqual, rest, 0),
            };
            (more, vec![rest, at])
        });
        let start = fx.b.ins().iadd(digits, state[1]);
        let len = fx.b.ins().isub(end, state[1]);
        self.bytes(fx, lo, start, len);
    }

    /// Writes a code point of a char or text literal whose quote is `quote`: `\` before the
    /// quote and `\`, a char from U+0020 to U+007E as itself, any other as `\u{H}`.
    fn code_point(self, fx: &mut Fx, lo: &mut Lowering, scalar: Value, quote: u8) {
        let is_quote =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, scalar, i64::from(quote));
        let is_slash =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, scalar, i64::from(b'\\'));
        let escaped = fx.b.ins().bor(is_quote, is_slash);
        fx.when(escaped, |fx| self.text(fx, lo, "\\"));
        let low =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, scalar, 0x20);
        let high =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedLessThanOrEqual, scalar, 0x7E);
        let printable = fx.b.ins().band(low, high);
        fx.when(printable, |fx| self.byte(fx, lo, scalar));
        let unicode = fx.b.ins().bxor_imm_s(printable, 1);
        fx.when(unicode, |fx| {
            self.text(fx, lo, "\\u{");
            let wide = super::super::body::widen(fx, scalar);
            self.hex(fx, lo, wide, None);
            self.text(fx, lo, "}");
        });
    }

    /// Writes a char as a char literal.
    fn char_literal(self, fx: &mut Fx, lo: &mut Lowering, scalar: Value) {
        self.text(fx, lo, "'");
        self.code_point(fx, lo, scalar, b'\'');
        self.text(fx, lo, "'");
    }

    /// Writes the UTF-8 text of a byte range as a text literal, or its bytes as a byte-string
    /// literal when `bytes` is not 0, a byte outside U+0020 to U+007E then written `\xHH`;
    /// stops once the report is full.
    fn literal(self, fx: &mut Fx, lo: &mut Lowering, start: Value, len: Value, bytes: Value) {
        let raw = fx.b.ins().icmp_imm_s(IntCC::NotEqual, bytes, 0);
        fx.when(raw, |fx| self.text(fx, lo, "b"));
        self.text(fx, lo, "\"");
        let any = fx.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
        fx.when(any, |fx| {
            let zero = fx.ptr(0);
            fx.repeat(&[zero], |fx, state| {
                let at = state[0];
                let place = fx.b.ins().iadd(start, at);
                let first = fx.load(types::I8, place, 0);
                let first = fx.b.ins().uextend(PTR, first);
                let step = fx.choose_in(
                    lo,
                    raw,
                    &[PTR],
                    |lo, fx| {
                        let low =
                            fx.b.ins()
                                .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, first, 0x20);
                        let high =
                            fx.b.ins()
                                .icmp_imm_s(IntCC::UnsignedLessThanOrEqual, first, 0x7E);
                        let printable = fx.b.ins().band(low, high);
                        fx.choose_in(
                            lo,
                            printable,
                            &[],
                            |lo, fx| {
                                self.code_point(fx, lo, first, b'"');
                                Vec::new()
                            },
                            |lo, fx| {
                                self.text(fx, lo, "\\x");
                                self.hex(fx, lo, first, Some(2));
                                Vec::new()
                            },
                        );
                        vec![fx.ptr(1)]
                    },
                    |lo, fx| {
                        let (scalar, step) = decode_utf8(fx, place, first);
                        self.code_point(fx, lo, scalar, b'"');
                        vec![step]
                    },
                );
                let next = fx.b.ins().iadd(at, step[0]);
                let more = fx.b.ins().icmp(IntCC::UnsignedLessThan, next, len);
                let full = self.full(fx);
                let room = fx.b.ins().bxor_imm_s(full, 1);
                let more = fx.b.ins().band(more, room);
                (more, vec![next])
            });
        });
        self.text(fx, lo, "\"");
    }

    /// Writes a float as `Render` writes it, from its bit pattern.
    fn float(self, fx: &mut Fx, lo: &mut Lowering, bits: Value, single: bool) {
        let value = match single {
            true => {
                let narrow = fx.b.ins().ireduce(types::I32, bits);
                fx.b.ins().bitcast(types::F32, MemFlagsData::new(), narrow)
            }
            false => fx.b.ins().bitcast(types::F64, MemFlagsData::new(), bits),
        };
        let buffer = fx.slot(40, 8);
        let name = if single { "f32_text" } else { "f64_text" };
        let len = lo
            .platform_call(fx, name, &[Held::Scalar(value), Held::Address(buffer)])
            .result
            .scalar();
        self.bytes(fx, lo, buffer, len);
    }

    /// Writes a scalar's bits as its writing says: a number or `bool` as `Render` writes it, a
    /// char as a char literal, or `_`.
    fn scalar(self, fx: &mut Fx, lo: &mut Lowering, bits: Value, how: Value) {
        let unsigned =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, how, Written::Unsigned as i64);
        fx.when(unsigned, |fx| self.decimal(fx, lo, bits));
        let signed =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, how, Written::Signed as i64);
        fx.when(signed, |fx| {
            let negative = fx.b.ins().icmp_imm_s(IntCC::SignedLessThan, bits, 0);
            fx.when(negative, |fx| self.text(fx, lo, "-"));
            let flipped = fx.b.ins().ineg(bits);
            let magnitude = fx.b.ins().select(negative, flipped, bits);
            self.decimal(fx, lo, magnitude);
        });
        let boolean =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, how, Written::Bool as i64);
        let truth = fx.b.ins().icmp_imm_s(IntCC::NotEqual, bits, 0);
        let true_bool = fx.b.ins().band(boolean, truth);
        fx.when(true_bool, |fx| self.text(fx, lo, "true"));
        let false_bool = fx.b.ins().bxor(boolean, true_bool);
        fx.when(false_bool, |fx| self.text(fx, lo, "false"));
        let character =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, how, Written::Char as i64);
        fx.when(character, |fx| self.char_literal(fx, lo, bits));
        let single =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, how, Written::F32 as i64);
        fx.when(single, |fx| self.float(fx, lo, bits, true));
        let double =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, how, Written::F64 as i64);
        fx.when(double, |fx| self.float(fx, lo, bits, false));
        let hidden =
            fx.b.ins()
                .icmp_imm_s(IntCC::Equal, how, Written::Hidden as i64);
        fx.when(hidden, |fx| self.text(fx, lo, "_"));
    }

    /// Writes the caller line of a location record.
    fn caller_line(self, fx: &mut Fx, lo: &mut Lowering, location: Value) {
        self.text(fx, lo, "    called from ");
        self.location_text(fx, lo, location, 0);
        self.text(fx, lo, ":");
        let line = fx.load(types::I32, location, 24);
        let line = fx.b.ins().uextend(PTR, line);
        self.decimal(fx, lo, line);
        self.text(fx, lo, " in ");
        self.location_text(fx, lo, location, 8);
        self.text(fx, lo, "\n");
    }

    /// Writes the text a location record holds at `field` (path, item or line).
    fn location_text(self, fx: &mut Fx, lo: &mut Lowering, location: Value, field: u64) {
        let tables = line_tables(lo, fx);
        let strings = fx.load(PTR, tables, 24);
        let offset = fx.load(types::I32, location, field);
        let offset = fx.b.ins().uextend(PTR, offset);
        let len = fx.load(types::I32, location, field + 4);
        let len = fx.b.ins().uextend(PTR, len);
        let start = fx.b.ins().iadd(strings, offset);
        self.bytes(fx, lo, start, len);
    }
}

/// The code point of the valid UTF-8 sequence whose first byte, `first`, is at `place`, and
/// the sequence's length.
fn decode_utf8(fx: &mut Fx, place: Value, first: Value) -> (Value, Value) {
    let two =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, first, 0xC0);
    let three =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, first, 0xE0);
    let four =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, first, 0xF0);
    let mut len = fx.ptr(1);
    for longer in [two, three, four] {
        let longer = super::super::body::widen(fx, longer);
        len = fx.b.ins().iadd(len, longer);
    }
    let point = fx.choose(
        two,
        &[PTR],
        |fx| {
            let three_bits = fx.ptr(0x0F);
            let two_bits = fx.ptr(0x1F);
            let lead = fx.b.ins().select(three, three_bits, two_bits);
            let four_bits = fx.ptr(0x07);
            let lead = fx.b.ins().select(four, four_bits, lead);
            let mut point = fx.b.ins().band(first, lead);
            for at in 1..4u64 {
                let inside =
                    fx.b.ins()
                        .icmp_imm_s(IntCC::UnsignedGreaterThan, len, at as i64);
                let joined = fx.choose(
                    inside,
                    &[PTR],
                    |fx| {
                        let byte = fx.load(types::I8, place, at);
                        let byte = fx.b.ins().uextend(PTR, byte);
                        let bits = fx.b.ins().band_imm_s(byte, 0x3F);
                        let shifted = fx.b.ins().ishl_imm_s(point, 6);
                        vec![fx.b.ins().bor(shifted, bits)]
                    },
                    |_| vec![point],
                )[0];
                point = joined;
            }
            vec![point]
        },
        |_| vec![first],
    )[0];
    (point, len)
}

/// The address of the line tables' header.
fn line_tables(lo: &mut Lowering, fx: &mut Fx) -> Value {
    let id = lo.lines.table(&mut lo.module);
    let global = lo.data_ref(fx, id);
    fx.b.ins().symbol_value(PTR, global)
}

/// The location record of a code address and whether its function's frames are left out, or
/// a zero location for an address no lowered function holds.
fn lookup(lo: &mut Lowering, fx: &mut Fx, address: Value) -> (Value, Value) {
    let tables = line_tables(lo, fx);
    let functions = fx.load(PTR, tables, 0);
    let entries = fx.load(PTR, tables, 8);
    let locations = fx.load(PTR, tables, 16);
    let count = fx.load(types::I32, tables, 32);
    let count = fx.b.ins().uextend(PTR, count);
    let zero = fx.ptr(0);
    let found = fx.repeat(&[zero, zero, zero], |fx, state| {
        let (index, location, hidden) = (state[0], state[1], state[2]);
        let record_offset = fx.b.ins().imul_imm_s(index, FUNCTION_RECORD as i64);
        let record = fx.b.ins().iadd(functions, record_offset);
        let start = fx.load(PTR, record, 0);
        let size = fx.load(types::I32, record, 8);
        let size = fx.b.ins().uextend(PTR, size);
        let offset = fx.b.ins().isub(address, start);
        let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, offset, size);
        let result = fx.choose(
            inside,
            &[PTR, PTR, types::I8],
            |fx| {
                let first = fx.load(types::I32, record, 12);
                let first = fx.b.ins().uextend(PTR, first);
                let entries_count = fx.load(types::I32, record, 16);
                let entries_count = fx.b.ins().uextend(PTR, entries_count);
                let flags = fx.load(types::I32, record, 20);
                let hidden = fx.b.ins().band_imm_s(flags, i64::from(lines::HIDDEN));
                let hidden = fx.b.ins().uextend(PTR, hidden);
                let pc = fx.b.ins().iadd_imm_s(offset, -1);
                let zero = fx.ptr(0);
                let best = fx.repeat(&[zero, zero], |fx, state| {
                    let (at, best) = (state[0], state[1]);
                    let index = fx.b.ins().iadd(first, at);
                    let entry_offset = fx.b.ins().imul_imm_s(index, ENTRY_RECORD as i64);
                    let entry = fx.b.ins().iadd(entries, entry_offset);
                    let code = fx.load(types::I32, entry, 0);
                    let code = fx.b.ins().uextend(PTR, code);
                    let number = fx.load(types::I32, entry, 4);
                    let number = fx.b.ins().uextend(PTR, number);
                    let covers = fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, code, pc);
                    let best = fx.b.ins().select(covers, number, best);
                    let at = fx.b.ins().iadd_imm_s(at, 1);
                    let more = fx.b.ins().icmp(IntCC::UnsignedLessThan, at, entries_count);
                    (more, vec![at, best])
                })[1];
                let location = location_record(fx, locations, best);
                let stop = fx.int(types::I8, 0);
                vec![location, hidden, stop]
            },
            |fx| {
                let go = fx.int(types::I8, 1);
                vec![location, hidden, go]
            },
        );
        let next = fx.b.ins().iadd_imm_s(index, 1);
        let more_functions = fx.b.ins().icmp(IntCC::UnsignedLessThan, next, count);
        let again = fx.b.ins().band(result[2], more_functions);
        (again, vec![next, result[0], result[1]])
    });
    (found[1], found[2])
}

/// The start of the lowered function holding a code address and its flags, or zeros for an
/// address outside lowered code.
pub fn function_at(lo: &mut Lowering, fx: &mut Fx, address: Value) -> (Value, Value) {
    let tables = line_tables(lo, fx);
    let functions = fx.load(PTR, tables, 0);
    let count = fx.load(types::I32, tables, 32);
    let count = fx.b.ins().uextend(PTR, count);
    let zero = fx.ptr(0);
    let found = fx.repeat(&[zero, zero, zero], |fx, state| {
        let (index, found_start, found_flags) = (state[0], state[1], state[2]);
        let offset = fx.b.ins().imul_imm_s(index, FUNCTION_RECORD as i64);
        let record = fx.b.ins().iadd(functions, offset);
        let start = fx.load(PTR, record, 0);
        let size = fx.load(types::I32, record, 8);
        let size = fx.b.ins().uextend(PTR, size);
        let from_start = fx.b.ins().isub(address, start);
        let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, from_start, size);
        let flags = fx.load(types::I32, record, 20);
        let flags = fx.b.ins().uextend(PTR, flags);
        let found_start = fx.b.ins().select(inside, start, found_start);
        let found_flags = fx.b.ins().select(inside, flags, found_flags);
        let next = fx.b.ins().iadd_imm_s(index, 1);
        let more = fx.b.ins().icmp(IntCC::UnsignedLessThan, next, count);
        let missing = fx.b.ins().bxor_imm_s(inside, 1);
        let more = fx.b.ins().band(more, missing);
        (more, vec![next, found_start, found_flags])
    });
    (found[1], found[2])
}

/// The address of the location record numbered `number`, or zero for number 0.
fn location_record(fx: &mut Fx, locations: Value, number: Value) -> Value {
    let has = fx.b.ins().icmp_imm_s(IntCC::NotEqual, number, 0);
    let index = fx.b.ins().iadd_imm_s(number, -1);
    let offset = fx.b.ins().imul_imm_s(index, LOCATION_RECORD as i64);
    let location = fx.b.ins().iadd(locations, offset);
    let zero = fx.ptr(0);
    fx.b.ins().select(has, location, zero)
}

/// `TrapRaise(kind, operands, count, caller)`: writes the report of a trap of the running task,
/// the location numbered `caller` its first caller line when not 0, and ends the process domain
/// with status 70.
fn raise(lo: &mut Lowering, fx: &mut Fx) {
    let params = fx.entry();
    let (kind, operands, count, caller) = (params[0], params[1], params[2], params[3]);
    let frames = fx.slot(FRAMES * 16, 8);
    let frame_pointer = fx.b.ins().get_frame_pointer(PTR);
    let zero = fx.ptr(0);
    let walked = fx.repeat(&[frame_pointer, zero, zero], |fx, state| {
        let (frame, at, beyond) = (state[0], state[1], state[2]);
        let address = fx.load(PTR, frame, 8);
        let (location, hidden) = lookup(lo, fx, address);
        let room =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedLessThan, at, FRAMES as i64);
        fx.when(room, |fx| {
            let slot_offset = fx.b.ins().imul_imm_s(at, 16);
            let slot = fx.b.ins().iadd(frames, slot_offset);
            fx.store(location, slot, 0);
            fx.store(hidden, slot, 8);
        });
        let known = fx.b.ins().icmp_imm_s(IntCC::NotEqual, location, 0);
        let shown = fx.b.ins().icmp_imm_s(IntCC::Equal, hidden, 0);
        let shown = fx.b.ins().band(shown, known);
        let full = fx.b.ins().bxor_imm_s(room, 1);
        let counted = fx.b.ins().band(shown, full);
        let counted = fx.b.ins().uextend(PTR, counted);
        let beyond = fx.b.ins().iadd(beyond, counted);
        let stored = fx.b.ins().uextend(PTR, room);
        let at = fx.b.ins().iadd(at, stored);
        let caller = fx.load(PTR, frame, 0);
        let has_caller = fx.b.ins().icmp_imm_s(IntCC::NotEqual, caller, 0);
        let more = fx.b.ins().band(known, has_caller);
        (more, vec![caller, at, beyond])
    });
    let (walked, beyond) = (walked[1], walked[2]);
    let frame_at = |fx: &mut Fx, index: Value| {
        let offset = fx.b.ins().imul_imm_s(index, 16);
        fx.b.ins().iadd(frames, offset)
    };
    let first_visible = fx.repeat(&[zero], |fx, state| {
        let index = state[0];
        let slot = frame_at(fx, index);
        let location = fx.load(PTR, slot, 0);
        let hidden = fx.load(PTR, slot, 8);
        let hidden = fx.b.ins().icmp_imm_s(IntCC::NotEqual, hidden, 0);
        let unknown = fx.b.ins().icmp_imm_s(IntCC::Equal, location, 0);
        let skip = fx.b.ins().bor(hidden, unknown);
        let next = fx.b.ins().iadd_imm_s(index, 1);
        let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, next, walked);
        let again = fx.b.ins().band(skip, inside);
        let index = fx.b.ins().select(again, next, index);
        (again, vec![index])
    })[0];
    let out = fx.slot(8 + REPORT, 8);
    fx.store(zero, out, 0);
    let report = Report { out };
    let kinds = lo.kind_names(fx);
    let kind_offset = fx.b.ins().imul_imm_s(kind, 16);
    let kind_record = fx.b.ins().iadd(kinds, kind_offset);
    let kind_text = fx.load(PTR, kind_record, 0);
    let kind_len = fx.load(PTR, kind_record, 8);
    report.bytes(fx, lo, kind_text, kind_len);
    let slot = frame_at(fx, first_visible);
    let location = fx.load(PTR, slot, 0);
    report.text(fx, lo, " at ");
    report.location_text(fx, lo, location, 0);
    report.text(fx, lo, ":");
    let line = fx.load(types::I32, location, 24);
    let line = fx.b.ins().uextend(PTR, line);
    report.decimal(fx, lo, line);
    report.text(fx, lo, " in ");
    report.location_text(fx, lo, location, 8);
    report.text(fx, lo, "\n    ");
    report.location_text(fx, lo, location, 16);
    report.text(fx, lo, "\n");
    let own = fx.b.ins().icmp_imm_s(IntCC::Equal, first_visible, 0);
    let shown = fx.b.ins().icmp_imm_s(IntCC::NotEqual, count, 0);
    let show = fx.b.ins().band(own, shown);
    let lines: Variable = fx.b.declare_var(PTR);
    let two = fx.ptr(2);
    fx.b.def_var(lines, two);
    fx.when(show, |fx| {
        report.text(fx, lo, "    ");
        let zero = fx.ptr(0);
        fx.each(zero, count, |fx, index| {
            let not_first = fx.b.ins().icmp_imm_s(IntCC::NotEqual, index, 0);
            fx.when(not_first, |fx| report.text(fx, lo, ", "));
            let offset = fx.b.ins().imul_imm_s(index, OPERAND_RECORD as i64);
            let record = fx.b.ins().iadd(operands, offset);
            let text = fx.load(PTR, record, 0);
            let text_len = fx.load(PTR, record, 8);
            report.bytes(fx, lo, text, text_len);
            report.text(fx, lo, " = ");
            let bits = fx.load(PTR, record, 16);
            let how = fx.load(PTR, record, 24);
            let writer = fx.load(PTR, record, 32);
            let glued = fx.b.ins().icmp_imm_s(IntCC::NotEqual, writer, 0);
            fx.choose_in(
                lo,
                glued,
                &[],
                |_, fx| {
                    let signature = fx.b.import_signature(writer_signature());
                    fx.b.ins().call_indirect(signature, writer, &[out, bits]);
                    Vec::new()
                },
                |lo, fx| {
                    lo.call_routine(fx, Routine::ReportScalar, &[out, bits, how]);
                    Vec::new()
                },
            );
        });
        report.text(fx, lo, "\n");
        let three = fx.ptr(3);
        fx.b.def_var(lines, three);
    });
    let named = fx.b.ins().icmp_imm_s(IntCC::NotEqual, caller, 0);
    fx.when(named, |fx| {
        let tables = line_tables(lo, fx);
        let locations = fx.load(PTR, tables, 16);
        let location = location_record(fx, locations, caller);
        report.caller_line(fx, lo, location);
        let written = fx.b.use_var(lines);
        let next = fx.b.ins().iadd_imm_s(written, 1);
        fx.b.def_var(lines, next);
    });
    let start = fx.b.ins().iadd_imm_s(first_visible, 1);
    let shown_at = |fx: &mut Fx, index: Value| {
        let slot = frame_at(fx, index);
        let location = fx.load(PTR, slot, 0);
        let hidden = fx.load(PTR, slot, 8);
        let visible = fx.b.ins().icmp_imm_s(IntCC::Equal, hidden, 0);
        let known = fx.b.ins().icmp_imm_s(IntCC::NotEqual, location, 0);
        (location, fx.b.ins().band(visible, known))
    };
    let callers = fx.b.declare_var(PTR);
    fx.b.def_var(callers, beyond);
    fx.each(start, walked, |fx, index| {
        let (_, shown) = shown_at(fx, index);
        let shown = fx.b.ins().uextend(PTR, shown);
        let counted = fx.b.use_var(callers);
        let counted = fx.b.ins().iadd(counted, shown);
        fx.b.def_var(callers, counted);
    });
    let callers = fx.b.use_var(callers);
    let written = fx.b.use_var(lines);
    let left = fx.ptr(LINES);
    let left = fx.b.ins().isub(left, written);
    let elided = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, callers, left);
    let fewer = fx.b.ins().iadd_imm_s(left, -1);
    let limit = fx.b.ins().select(elided, fewer, left);
    let written_callers = fx.b.declare_var(PTR);
    let none = fx.ptr(0);
    fx.b.def_var(written_callers, none);
    fx.each(start, walked, |fx, index| {
        let (location, shown) = shown_at(fx, index);
        let count = fx.b.use_var(written_callers);
        let room = fx.b.ins().icmp(IntCC::UnsignedLessThan, count, limit);
        let shown = fx.b.ins().band(shown, room);
        fx.when(shown, |fx| {
            report.caller_line(fx, lo, location);
            let next = fx.b.ins().iadd_imm_s(count, 1);
            fx.b.def_var(written_callers, next);
        });
    });
    fx.when(elided, |fx| {
        report.text(fx, lo, "    called from ");
        let left_out = fx.b.ins().isub(callers, limit);
        report.decimal(fx, lo, left_out);
        report.text(fx, lo, " more frames\n");
    });
    let task = tasks::current_task(lo, fx);
    let domain = tasks::current_domain(lo, fx);
    let state_address = lo.state(fx);
    let process = fx.load(PTR, state_address, state::PROCESS);
    let inside = fx.b.ins().icmp_imm_s(IntCC::NotEqual, task, 0);
    let nested = fx.b.ins().icmp(IntCC::NotEqual, domain, process);
    let contained = fx.b.ins().band(inside, nested);
    fx.when(contained, |fx| abandon(lo, fx, task, domain, kind, out));
    let bytes = fx.b.ins().iadd_imm_s(out, 8);
    let length = fx.load(PTR, out, 0);
    lo.call_routine(fx, Routine::WriteErr, &[bytes, length]);
    let status = fx.ptr(70);
    lo.call_routine(fx, Routine::Exit, &[status]);
    fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
}

/// Abandons the `sys::Domain` body domain of a trapping task: the first trap of the domain
/// writes its report into the domain's trap record and wakes the task waiting for the domain,
/// which settles it; the trapping task releases the locks its thread holds for it and stops.
fn abandon(lo: &mut Lowering, fx: &mut Fx, task: Value, domain: Value, kind: Value, out: Value) {
    let flag = fx.offset(domain, tasks::domain::ABANDONED);
    let zero = fx.ptr(0);
    let one = fx.ptr(1);
    let seen =
        fx.b.ins()
            .atomic_cas(MemFlagsData::trusted(), flag, zero, one);
    let first = fx.b.ins().icmp_imm_s(IntCC::Equal, seen, 0);
    fx.when(first, |fx| {
        let record = fx.load(PTR, domain, tasks::domain::TRAP);
        fx.store(kind, record, trap_record::KIND);
        let len = fx.load(PTR, out, 0);
        fx.store(len, record, trap_record::LEN);
        let target = fx.offset(record, trap_record::BYTES);
        let bytes = fx.b.ins().iadd_imm_s(out, 8);
        lo.call_routine(fx, Routine::Copy, &[target, bytes, len]);
        let lock = fx.offset(domain, tasks::domain::LOCK);
        lo.call_routine(fx, Routine::Lock, &[lock]);
        let waiter = fx.load(PTR, domain, tasks::domain::WAITER);
        lo.call_routine(fx, Routine::Unlock, &[lock]);
        lo.call_routine(fx, Routine::Wake, &[waiter]);
    });
    super::super::host::release_locks(lo, fx);
    tasks::stop_running(lo, fx, task);
}

/// Traps with a kind and no operands, from code lowering writes itself.
pub fn raise_kind(lo: &mut Lowering, fx: &mut Fx, kind: &str) {
    let kind = fx.ptr(super::super::trap::kind_number(kind) as u64);
    let zero = fx.ptr(0);
    lo.call_routine(fx, Routine::TrapRaise, &[kind, zero, zero, zero]);
    fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
}

impl Lowering<'_, '_> {
    /// The address of the table of trap kind names: each the address and length of its text.
    fn kind_names(&mut self, fx: &mut Fx) -> Value {
        let text: String = KINDS.concat();
        let strings = self.bytes_data(text.as_bytes());
        let mut record = Vec::new();
        let mut relocations = Vec::new();
        let mut at = 0u64;
        for (index, kind) in KINDS.iter().enumerate() {
            relocations.push((index as u32 * 16, at));
            record.extend_from_slice(&0u64.to_le_bytes());
            record.extend_from_slice(&(kind.len() as u64).to_le_bytes());
            at += kind.len() as u64;
        }
        let id = self.relocated_data("kinds", record, &relocations, strings);
        let global = self.data_ref(fx, id);
        fx.b.ins().symbol_value(PTR, global)
    }
}
