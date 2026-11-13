//! Decoding of NIR streams: whether a stream is the canonical encoding of units under the NIR
//! version it names. A value of a struct, enum or layout type is read only as part of its field:
//! its encoding follows declarations the stream need not hold.

use super::encode::{MAGIC, VERSION};
use crate::artefact::{read_bytes, read_uvar};
use crate::diagnostic::Code;

/// Why a NIR stream does not decode: a version other than the one this toolchain implements, or
/// bytes that are not the canonical encoding of units; none when it decodes.
pub fn defect(stream: &[u8]) -> Option<Code> {
    let Some(rest) = stream.strip_prefix(&MAGIC[..]) else {
        return Some(Code::NirMalformed);
    };
    let mut r = Reader::new(rest);
    let version = r.number();
    if r.failed {
        return Some(Code::NirMalformed);
    }
    if version != VERSION {
        return Some(Code::NirVersionUnsupported);
    }
    let mut previous: Option<(&str, &str)> = None;
    for _ in 0..r.count() {
        let order = unit(&mut r);
        r.need(previous < Some(order));
        previous = Some(order);
    }
    (!r.done()).then_some(Code::NirMalformed)
}

/// Encoded bytes being read: where the next value starts, where the record field being read
/// ends, and whether every value read so far was well formed. A failed reader reads every value
/// as 0 or empty.
#[derive(Clone, Copy)]
struct Reader<'s> {
    bytes: &'s [u8],
    at: usize,
    end: usize,
    failed: bool,
}

/// The fields of a record being read: how many are left, the greatest key the record defines,
/// the last key read, the keys read as bits, and where the record's enclosing field ends.
struct Fields {
    left: u64,
    keys: u64,
    last: u64,
    seen: u64,
    outer: usize,
}

impl Fields {
    fn has(&self, key: u64) -> bool {
        self.seen & 1 << key != 0
    }

    /// Whether it holds every key from 1 to `last`.
    fn through(&self, last: u64) -> bool {
        (1..=last).all(|key| self.has(key))
    }
}

impl<'s> Reader<'s> {
    fn new(bytes: &'s [u8]) -> Reader<'s> {
        Reader {
            bytes,
            at: 0,
            end: bytes.len(),
            failed: false,
        }
    }

    fn need(&mut self, holds: bool) {
        self.failed |= !holds;
    }

    /// Whether every value read was well formed and none is left.
    fn done(&self) -> bool {
        !self.failed && self.at == self.end
    }

    /// A uvar within 64 bits.
    fn number(&mut self) -> u64 {
        let read = read_uvar(&self.bytes[self.at..self.end])
            .and_then(|(value, rest)| Some((u64::try_from(value).ok()?, rest)));
        match read {
            Some((value, rest)) if !self.failed => {
                self.at = self.end - rest.len();
                value
            }
            _ => {
                self.failed = true;
                0
            }
        }
    }

    /// A choice among `count` alternatives, by its position.
    fn choice(&mut self, count: u64) -> u64 {
        let position = self.number();
        self.need(position < count);
        if self.failed { 0 } else { position }
    }

    fn flag(&mut self) -> bool {
        self.choice(2) == 1
    }

    /// The count of a list whose elements each take a byte at least.
    fn count(&mut self) -> u64 {
        let count = self.number();
        self.need(count <= (self.end - self.at) as u64);
        if self.failed { 0 } else { count }
    }

    /// The count of a list the encoding omits when it is empty.
    fn nonempty(&mut self) -> u64 {
        let count = self.count();
        self.need(count > 0);
        count
    }

    fn bytes(&mut self) -> &'s [u8] {
        match read_bytes(&self.bytes[self.at..self.end]) {
            Some((held, rest)) if !self.failed => {
                self.at = self.end - rest.len();
                held
            }
            _ => {
                self.failed = true;
                &[]
            }
        }
    }

    fn text(&mut self) -> &'s str {
        let held = self.bytes();
        let text = std::str::from_utf8(held).unwrap_or_default();
        self.need(text.len() == held.len());
        text
    }

    /// `width` bytes as they stand.
    fn fixed(&mut self, width: usize) {
        self.need(self.end - self.at >= width);
        if !self.failed {
            self.at += width;
        }
    }

    /// The fields of the record at the reader, whose keys run from 1 to `keys`.
    fn fields(&mut self, keys: u64) -> Fields {
        Fields {
            left: self.count(),
            keys,
            last: 0,
            seen: 0,
            outer: self.end,
        }
    }

    /// Enters the next field of a record, whose key is then the fields' last; false when none is
    /// left.
    fn field(&mut self, fields: &mut Fields) -> bool {
        if fields.left == 0 || self.failed {
            return false;
        }
        fields.left -= 1;
        let key = self.number();
        self.need(fields.last < key && key <= fields.keys);
        let length = self.number();
        self.need(length <= (self.end - self.at) as u64);
        if self.failed {
            return false;
        }
        fields.last = key;
        fields.seen |= 1 << key;
        self.end = self.at + length as usize;
        true
    }

    /// Leaves a field, whose content must have been read whole.
    fn close(&mut self, fields: &Fields) {
        self.need(self.at == self.end);
        self.end = fields.outer;
    }
}

/// What reading a unit's spans and digests needs of a function: its role, its parameter count
/// and, for each block, whether each instruction is a slot.
struct Skeleton {
    role: u64,
    params: u64,
    blocks: Vec<Vec<bool>>,
}

/// How many functions part 9 covers: those of the roles `body`, `closure` and `default`.
fn covered_count(functions: &[Skeleton]) -> u64 {
    functions
        .iter()
        .filter(|function| matches!(function.role, 0 | 2 | 12))
        .count() as u64
}

/// A unit, giving its path and first target, which order a stream's units.
fn unit<'s>(r: &mut Reader<'s>) -> (&'s str, &'s str) {
    let mut path = "";
    let mut targets = Vec::new();
    let mut functions = Vec::new();
    let mut fields = r.fields(12);
    while r.field(&mut fields) {
        match fields.last {
            1 => path = r.text(),
            2 => _ = r.choice(15),
            3 => {
                targets = sorted_texts(r);
                r.need(!targets.is_empty());
            }
            4 => visibility(r),
            5 => {
                let head = head(r, false);
                r.need(head != 0);
            }
            6 => declaration(r),
            7 => properties(r),
            8 => {
                for _ in 0..r.nonempty() {
                    functions.push(function(r));
                }
            }
            9 => digests(r, &targets, covered_count(&functions)),
            10 => {
                for _ in 0..r.nonempty() {
                    r.text();
                }
            }
            11 => {
                for _ in 0..r.nonempty() {
                    r.choice(4);
                }
            }
            _ => spans(r, &functions),
        }
        r.close(&fields);
    }
    r.need(fields.through(4));
    r.need(fields.has(8) == fields.has(12) && fields.has(9) == (covered_count(&functions) > 0));
    (path, targets.first().copied().unwrap_or_default())
}

/// A list of texts in strictly increasing byte order.
fn sorted_texts<'s>(r: &mut Reader<'s>) -> Vec<&'s str> {
    let mut texts: Vec<&str> = Vec::new();
    for _ in 0..r.count() {
        let text = r.text();
        r.need(texts.last().is_none_or(|last| *last < text));
        texts.push(text);
    }
    texts
}

/// An optional word of the `visibility` rule.
fn visibility(r: &mut Reader) {
    if r.flag() {
        r.choice(2);
    }
}

/// A head as part 5 writes it, giving the keys it holds as bits; a function's head holds its
/// result and wait flag.
fn head(r: &mut Reader, function: bool) -> u64 {
    let mut fields = r.fields(9);
    while r.field(&mut fields) {
        match fields.last {
            1 => {
                for _ in 0..r.nonempty() {
                    r.choice(11);
                }
            }
            2 => {
                for _ in 0..r.nonempty() {
                    generic(r);
                }
            }
            3 => {
                for _ in 0..r.nonempty() {
                    parameter(r);
                }
            }
            4 | 5 => _ = ty(r),
            6 | 9 => _ = r.flag(),
            7 => limit_clause(r),
            _ => _ = r.text(),
        }
        r.close(&fields);
    }
    let declared = fields.seen >> 3 != 0;
    r.need(!(function || declared) || fields.has(4) && fields.has(6));
    fields.seen
}

/// A generic parameter: its name, then its bounds or a const parameter's type.
fn generic(r: &mut Reader) {
    let mut fields = r.fields(3);
    while r.field(&mut fields) {
        match fields.last {
            1 => _ = r.text(),
            2 => {
                for _ in 0..r.nonempty() {
                    ty(r);
                }
            }
            _ => _ = ty(r),
        }
        r.close(&fields);
    }
    r.need(fields.has(1));
}

/// A parameter of a head: its mode, name and type.
fn parameter(r: &mut Reader) {
    let mut fields = r.fields(3);
    while r.field(&mut fields) {
        match fields.last {
            1 => _ = r.choice(3),
            2 => _ = r.text(),
            _ => _ = ty(r),
        }
        r.close(&fields);
    }
    r.need(fields.through(3));
}

/// A limits clause: its entries in the order of the `limit` rule, each its word and `stack`'s
/// size or the class.
fn limit_clause(r: &mut Reader) {
    let mut next = 0;
    for _ in 0..r.nonempty() {
        let mut word = 6;
        let mut fields = r.fields(2);
        while r.field(&mut fields) {
            match (fields.last, word) {
                (1, _) => word = r.choice(6),
                (_, 4) => _ = r.number(),
                (_, 5) => _ = class(r),
                _ => r.need(false),
            }
            r.close(&fields);
        }
        r.need(fields.has(1) && fields.has(2) == (word >= 4) && word >= next);
        next = word + 1;
    }
}

/// A reversibility class, giving its position: for an irreversible class, its kinds follow.
fn class(r: &mut Reader) -> u64 {
    let class = r.choice(4);
    if class == 3 {
        let mut next = 0;
        for _ in 0..r.count() {
            let kind = r.choice(4);
            r.need(kind >= next);
            next = kind + 1;
        }
    }
    class
}

/// What decoding a value needs of a type: its constructor, and for a named type its path, the
/// encoding of its arguments and their count.
enum Shape<'s> {
    Scalar(u64),
    Unit,
    Named(&'s str, &'s [u8], u64),
    Fn,
    Other,
}

fn ty<'s>(r: &mut Reader<'s>) -> Shape<'s> {
    match r.choice(9) {
        0 => Shape::Scalar(r.choice(14)),
        1 => Shape::Unit,
        3 => {
            let path = r.text();
            let count = r.count();
            let start = r.at;
            for _ in 0..count {
                ty(r);
            }
            Shape::Named(path, &r.bytes[start..r.at], count)
        }
        4 => {
            r.number();
            Shape::Other
        }
        5 => {
            literal(r);
            Shape::Other
        }
        6 => {
            r.choice(3);
            head(r, true);
            Shape::Fn
        }
        7 | 8 => {
            ty(r);
            Shape::Other
        }
        _ => Shape::Other,
    }
}

/// A literal: its type, then its value.
fn literal(r: &mut Reader) {
    match ty(r) {
        Shape::Scalar(scalar) => scalar_value(r, scalar),
        Shape::Unit => {}
        Shape::Named("core::Str", _, 0) => _ = r.text(),
        Shape::Named("core::Slice", [0, 0], 1) => _ = r.bytes(),
        _ => r.need(false),
    }
}

/// The width of an integer type, by its position in the scalar table.
fn width(scalar: u64) -> u64 {
    match scalar {
        0 | 4 => 8,
        1 | 5 => 16,
        2 | 6 => 32,
        _ => 64,
    }
}

/// A value of a scalar type, by its position in the scalar table: an integer's uvar or svar
/// below 2 to the power of its width, a float's bits, a `bool` or a `char`.
fn scalar_value(r: &mut Reader, scalar: u64) {
    match scalar {
        10 => r.fixed(4),
        11 => r.fixed(8),
        12 => _ = r.flag(),
        13 => {
            let value = r.number();
            r.need(u32::try_from(value).ok().and_then(char::from_u32).is_some());
        }
        _ => {
            let width = width(scalar);
            let value = r.number();
            r.need(width == 64 || value >> width == 0);
        }
    }
}

/// A const's value, following its type; false when it reaches a value whose encoding follows a
/// declaration.
fn value(r: &mut Reader, shape: &Shape) -> bool {
    match shape {
        Shape::Scalar(scalar) => scalar_value(r, *scalar),
        Shape::Unit => {}
        Shape::Fn => fn_ref(r),
        Shape::Named(path, args, count) => match *path {
            "hw::Ptr" | "hw::Register" | "mem::Id" | "hw::UserAddr" => _ = r.number(),
            "core::Str" | "text::Fixed" | "core::CharCursor" => _ = r.text(),
            "core::Array" | "core::Slice" | "mem::Bounded" | "core::SliceCursor"
            | "core::ArrayCursor" | "mem::BoundedCursor" => {
                let element = element(r, args, *count);
                for _ in 0..r.number() {
                    let start = r.at;
                    if !value(r, &element) {
                        return false;
                    }
                    if r.failed || r.at == start {
                        break;
                    }
                }
            }
            "core::Secret" | "core::Untrusted" => {
                let held = element(r, args, *count);
                return value(r, &held);
            }
            _ => return false,
        },
        Shape::Other => return false,
    }
    true
}

/// The type of a value's elements or of the value it holds: its type's first argument.
fn element<'s>(r: &mut Reader, args: &'s [u8], count: u64) -> Shape<'s> {
    r.need(count > 0);
    ty(&mut Reader::new(args))
}

/// A declaration: what its item declares beyond its head.
fn declaration(r: &mut Reader) {
    let mut fields = r.fields(11);
    while r.field(&mut fields) {
        match fields.last {
            1 => {
                for _ in 0..r.count() {
                    struct_field(r);
                }
            }
            2 => {
                for _ in 0..r.count() {
                    variant(r);
                }
            }
            3 => layout_record(r),
            4 => {
                for _ in 0..r.count() {
                    register(r);
                }
            }
            5 => {
                for _ in 0..r.count() {
                    method(r);
                }
            }
            6 => implementation(r),
            7 => registry_record(r),
            8 => contribution(r),
            9 => constant(r),
            10 => _ = r.text(),
            _ => protocol(r),
        }
        r.close(&fields);
    }
    r.need(fields.seen != 0);
}

fn struct_field(r: &mut Reader) {
    let mut fields = r.fields(3);
    while r.field(&mut fields) {
        match fields.last {
            1 => visibility(r),
            2 => _ = r.text(),
            _ => _ = ty(r),
        }
        r.close(&fields);
    }
    r.need(fields.through(3));
}

fn variant(r: &mut Reader) {
    let mut fields = r.fields(3);
    while r.field(&mut fields) {
        match fields.last {
            1 => _ = r.text(),
            2 => match r.choice(3) {
                1 => _ = ty(r),
                2 => {
                    for _ in 0..r.count() {
                        r.text();
                        ty(r);
                    }
                }
                _ => {}
            },
            _ => {
                let from = r.number();
                r.need(from == 1);
            }
        }
        r.close(&fields);
    }
    r.need(fields.through(2));
}

fn layout_record(r: &mut Reader) {
    let mut fields = r.fields(3);
    while r.field(&mut fields) {
        match fields.last {
            1 => {
                if r.flag() {
                    r.text();
                } else {
                    r.number();
                }
                if r.flag() {
                    r.choice(2);
                }
            }
            2 => {
                for _ in 0..r.nonempty() {
                    layout_field(r);
                }
            }
            _ => {
                for _ in 0..r.count() {
                    r.text();
                    r.number();
                }
            }
        }
        r.close(&fields);
    }
    r.need(fields.has(1));
}

fn layout_field(r: &mut Reader) {
    let mut fields = r.fields(2);
    while r.field(&mut fields) {
        match fields.last {
            1 => _ = r.text(),
            _ => match r.choice(3) {
                0 => {
                    r.number();
                    if r.flag() {
                        r.number();
                    }
                }
                2 => {
                    ty(r);
                    r.number();
                }
                _ => {}
            },
        }
        r.close(&fields);
    }
    r.need(fields.through(2));
}

fn register(r: &mut Reader) {
    let mut fields = r.fields(5);
    while r.field(&mut fields) {
        match fields.last {
            1 => _ = r.text(),
            2 => _ = ty(r),
            3 | 4 => _ = r.number(),
            _ => _ = r.choice(4),
        }
        r.close(&fields);
    }
    r.need(fields.through(3) && fields.has(5));
}

fn method(r: &mut Reader) {
    let mut fields = r.fields(3);
    while r.field(&mut fields) {
        match fields.last {
            1 => _ = r.text(),
            2 => _ = head(r, true),
            _ => _ = r.flag(),
        }
        r.close(&fields);
    }
    r.need(fields.through(3));
}

fn implementation(r: &mut Reader) {
    let mut fields = r.fields(2);
    while r.field(&mut fields) {
        ty(r);
        r.close(&fields);
    }
    r.need(fields.has(1));
}

fn registry_record(r: &mut Reader) {
    let mut fields = r.fields(4);
    while r.field(&mut fields) {
        match fields.last {
            1 => _ = ty(r),
            2 => _ = r.text(),
            3 => {
                for _ in 0..r.nonempty() {
                    r.text();
                }
            }
            _ => {
                let loaded = r.number();
                r.need(loaded == 1);
            }
        }
        r.close(&fields);
    }
    r.need(fields.through(2));
}

fn contribution(r: &mut Reader) {
    let mut fields = r.fields(2);
    while r.field(&mut fields) {
        r.text();
        r.close(&fields);
    }
    r.need(fields.through(2));
}

/// A const: its type, then its value, read up to a value whose encoding follows a declaration.
fn constant(r: &mut Reader) {
    let mut shape = None;
    let mut fields = r.fields(2);
    while r.field(&mut fields) {
        match (fields.last, &shape) {
            (1, _) => shape = Some(ty(r)),
            (_, Some(shape)) => {
                if !value(r, shape) {
                    r.at = r.end;
                }
            }
            _ => r.need(false),
        }
        r.close(&fields);
    }
    r.need(fields.has(1));
}

fn protocol(r: &mut Reader) {
    let mut fields = r.fields(2);
    while r.field(&mut fields) {
        match fields.last {
            1 => _ = ty(r),
            _ => {
                for _ in 0..r.count() {
                    operation(r);
                }
            }
        }
        r.close(&fields);
    }
    r.need(fields.has(2));
}

fn operation(r: &mut Reader) {
    let mut fields = r.fields(3);
    while r.field(&mut fields) {
        match fields.last {
            1 => visibility(r),
            2 => _ = r.text(),
            _ => _ = head(r, true),
        }
        r.close(&fields);
    }
    r.need(fields.through(3));
}

/// A declared type's structural properties, or an interface's Dyn-usability.
fn properties(r: &mut Reader) {
    let mut fields = r.fields(7);
    while r.field(&mut fields) {
        r.flag();
        r.close(&fields);
    }
    r.need(fields.seen != 0);
}

fn function(r: &mut Reader) -> Skeleton {
    let mut skeleton = Skeleton {
        role: 0,
        params: 0,
        blocks: Vec::new(),
    };
    let mut fields = r.fields(8);
    while r.field(&mut fields) {
        match fields.last {
            1 => skeleton.role = r.choice(13),
            2 => _ = r.number(),
            3 => {
                skeleton.params = r.count();
                for _ in 0..skeleton.params {
                    r.choice(3);
                    ty(r);
                }
            }
            4 | 5 => _ = ty(r),
            6 => _ = r.flag(),
            7 => {
                for _ in 0..r.count() {
                    skeleton.blocks.push(block(r));
                }
            }
            _ => closure(r),
        }
        r.close(&fields);
    }
    r.need(fields.through(4) && fields.has(6) && fields.has(7));
    skeleton
}

/// A closure's kind and limits.
fn closure(r: &mut Reader) {
    let mut fields = r.fields(2);
    while r.field(&mut fields) {
        match fields.last {
            1 => _ = r.flag(),
            _ => limit_clause(r),
        }
        r.close(&fields);
    }
    r.need(fields.has(1));
}

/// A block: its parameter types, its instructions and its terminator, giving whether each
/// instruction is a slot.
fn block(r: &mut Reader) -> Vec<bool> {
    for _ in 0..r.count() {
        ty(r);
    }
    let slots = (0..r.count()).map(|_| inst(r)).collect();
    terminator(r);
    slots
}

/// An instruction: its opcode, its premise marker and its parts, giving whether it is a slot.
fn inst(r: &mut Reader) -> bool {
    let opcode = r.number();
    r.number();
    match opcode {
        0 => _ = ty(r),
        1 | 2 | 6 => place(r),
        3 => {
            r.flag();
            place(r);
        }
        4 | 5 => {
            place(r);
            operand(r);
        }
        7 => fn_ref(r),
        8 => {
            if r.flag() {
                r.number();
            } else {
                r.text();
            }
        }
        9 => {
            ty(r);
            operands(r);
        }
        10 => {
            ty(r);
            r.text();
            match r.choice(3) {
                1 => operand(r),
                2 => operands(r),
                _ => {}
            }
        }
        11 => {
            place(r);
            ty(r);
            r.text();
        }
        12 => {
            r.number();
            operands(r);
        }
        13 => {
            operand(r);
            ty(r);
            if r.flag() {
                r.text();
            }
        }
        14 | 25 | 32 | 37 | 41 => operand(r),
        15..=24 | 26..=31 => {
            operand(r);
            operand(r);
        }
        33..=35 => call(r, opcode),
        36 => {
            r.choice(6);
            operand(r);
        }
        38 | 39 => {
            operand(r);
            r.flag();
            operand(r);
            region(r);
        }
        40 => {
            r.flag();
            operands(r);
            region(r);
        }
        _ => r.need(false),
    }
    opcode == 0
}

fn terminator(r: &mut Reader) {
    match r.number() {
        42 => target(r),
        43 => {
            operand(r);
            target(r);
            target(r);
        }
        44 => {
            let opcode = r.number();
            r.need((33..=35).contains(&opcode));
            r.number();
            call(r, opcode);
            r.number();
            r.number();
        }
        45 => {
            if r.flag() {
                operand(r);
            }
        }
        46 | 49 => operand(r),
        47 => _ = r.text(),
        48 => {}
        _ => r.need(false),
    }
}

/// A call's parts after its opcode: `await`, its callee and its operands.
fn call(r: &mut Reader, opcode: u64) {
    r.flag();
    match opcode {
        33 => fn_ref(r),
        34 => _ = r.number(),
        _ => {
            place(r);
            r.text();
            r.text();
        }
    }
    operands(r);
}

fn target(r: &mut Reader) {
    r.number();
    operands(r);
}

fn operand(r: &mut Reader) {
    match r.choice(3) {
        0 => _ = r.number(),
        1 => literal(r),
        _ => {
            r.choice(3);
            place(r);
        }
    }
}

fn operands(r: &mut Reader) {
    for _ in 0..r.count() {
        operand(r);
    }
}

fn place(r: &mut Reader) {
    r.number();
    for _ in 0..r.count() {
        r.text();
    }
}

fn region(r: &mut Reader) {
    for _ in 0..r.count() {
        r.number();
    }
}

/// A function reference: its canonical path, role and index, and type arguments.
fn fn_ref(r: &mut Reader) {
    r.text();
    r.choice(13);
    r.number();
    for _ in 0..r.count() {
        ty(r);
    }
}

/// Part 9: for each covered function, its digest under each of the unit's targets.
fn digests(r: &mut Reader, targets: &[&str], covered: u64) {
    let count = r.count();
    r.need(count == covered);
    for _ in 0..count {
        let pairs = r.count();
        r.need(pairs == targets.len() as u64);
        for at in 0..pairs {
            let target = r.text();
            r.need(targets.get(at as usize) == Some(&target));
            digest(r);
        }
    }
}

fn digest(r: &mut Reader) {
    let mut fields = r.fields(11);
    while r.field(&mut fields) {
        match fields.last {
            1..=8 => _ = r.flag(),
            9 | 10 => {
                if r.choice(2) == 0 {
                    sorted_texts(r);
                }
            }
            _ => {
                let class = class(r);
                r.need(class != 0);
            }
        }
        r.close(&fields);
    }
    r.need(fields.through(10));
}

/// Part 12: for each function, its span, its parameters' spans with their names, then each
/// block's instructions' spans, a binding's slot's with its name, and its terminator's span.
fn spans(r: &mut Reader, functions: &[Skeleton]) {
    let count = r.count();
    r.need(count == functions.len() as u64);
    for function in functions {
        span(r);
        let params = r.count();
        r.need(params == function.params);
        for _ in 0..params {
            span(r);
            r.text();
        }
        let blocks = r.count();
        r.need(blocks == function.blocks.len() as u64);
        for slots in &function.blocks {
            let insts = r.count();
            r.need(insts == slots.len() as u64);
            for slot in slots {
                span(r);
                if *slot {
                    slot_name(r);
                }
            }
            span(r);
        }
    }
}

/// A span: its path, which holds a `/`, and its four positions.
fn span(r: &mut Reader) {
    let path = r.text();
    r.need(path.contains('/'));
    for _ in 0..4 {
        r.number();
    }
}

/// A slot's name, when the text at the reader is one rather than the next span's path.
fn slot_name(r: &mut Reader) {
    let mut ahead = *r;
    let name = ahead.text();
    if !ahead.failed && !name.contains('/') {
        *r = ahead;
    }
}

#[cfg(test)]
mod tests;
