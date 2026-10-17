//! The simulated file system: its files by joined path, each holding bytes drawn through the
//! file system's allocator and counted against its size limit, and its open files, each the
//! file it reads or writes and its position. Directories are implicit.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::super::Lowering;
use super::super::super::body::Held;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::Routine;
use super::super::super::runtime::tables::entry;
use super::super::Supply;
use super::super::mem::vector;
use super::super::sys;
use super::{cancellation_point, draw, new_object, object, object_at};

/// The offsets of a file system's fields: its size limit, its files' total size, the cell it
/// draws through and its first file.
mod system {
    pub const LIMIT: u64 = 8;
    pub const TOTAL: u64 = 16;
    pub const CELL: u64 = 24;
    pub const FILES: u64 = 32;
    pub const SIZE: u64 = 40;
}

/// The offsets of a file's fields: the next file, its joined path and its bytes.
mod file {
    pub const NEXT: u64 = 0;
    pub const PATH: u64 = 8;
    pub const PATH_LEN: u64 = 16;
    pub const DATA: u64 = 24;
    pub const LEN: u64 = 32;
    pub const SIZE: u64 = 40;
}

/// The offsets of an open file's fields: its file system, its file, its position and whether
/// it was opened for writing.
mod open {
    pub const SYSTEM: u64 = 8;
    pub const FILE: u64 = 16;
    pub const POSITION: u64 = 24;
    pub const WRITING: u64 = 32;
    pub const SIZE: u64 = 40;
}

/// A new empty file system of a size limit drawing through a cell: its index.
pub fn new(lo: &mut Lowering, fx: &mut Fx, limit: Value, cell: Value) -> Value {
    let (index, record) = new_object(lo, fx, object::FS, system::SIZE);
    fx.store(limit, record, system::LIMIT);
    fx.store(cell, record, system::CELL);
    index
}

/// The file system an entry names, and its path prefix.
fn of_entry(lo: &mut Lowering, fx: &mut Fx, record: Value) -> (Value, (Value, Value)) {
    let index = fx.load(PTR, record, entry::STATE);
    let system = object_at(lo, fx, index);
    let start = fx.load(PTR, record, entry::STATE + 8);
    let len = fx.load(PTR, record, entry::STATE + 16);
    (system, (start, len))
}

/// The joined path of a prefix and a path in runtime memory: the prefix, `/` when it is not
/// empty, and the path.
fn joined(lo: &mut Lowering, fx: &mut Fx, prefix: (Value, Value), path: Held) -> (Value, Value) {
    let (path_start, path_len) = sys::view(fx, path);
    let has_prefix = fx.b.ins().icmp_imm_s(IntCC::NotEqual, prefix.1, 0);
    let separator = fx.b.ins().uextend(PTR, has_prefix);
    let len = fx.b.ins().iadd(prefix.1, separator);
    let len = fx.b.ins().iadd(len, path_len);
    let size = fx.b.ins().iadd_imm_s(len, 1);
    let text = lo.record_alloc(fx, size);
    super::exhausted_unless(lo, fx, text);
    lo.call_routine(fx, Routine::Copy, &[text, prefix.0, prefix.1]);
    fx.when(has_prefix, |fx| {
        let at = fx.b.ins().iadd(text, prefix.1);
        let slash = fx.int(types::I8, i64::from(b'/'));
        fx.store(slash, at, 0);
    });
    let tail = fx.b.ins().iadd(text, prefix.1);
    let tail = fx.b.ins().iadd(tail, separator);
    lo.call_routine(fx, Routine::Copy, &[tail, path_start, path_len]);
    (text, len)
}

/// The file of a joined path, with the link pointing at it, or zeros when none.
fn find(lo: &mut Lowering, fx: &mut Fx, system: Value, path: (Value, Value)) -> (Value, Value) {
    let link = fx.offset(system, system::FILES);
    let first = fx.load(PTR, link, 0);
    let found = fx.repeat(&[link, first], |fx, state| {
        let (link, current) = (state[0], state[1]);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let same = fx.choose_in(
            lo,
            present,
            &[types::I8],
            |lo, fx| {
                let len = fx.load(PTR, current, file::PATH_LEN);
                let same_len = fx.b.ins().icmp(IntCC::Equal, len, path.1);
                let same = fx.choose_in(
                    lo,
                    same_len,
                    &[types::I8],
                    |lo, fx| {
                        let text = fx.load(PTR, current, file::PATH);
                        let order = lo.call_routine(fx, Routine::Compare, &[text, path.0, len]);
                        vec![fx.b.ins().icmp_imm_s(IntCC::Equal, order[0], 0)]
                    },
                    |_, fx| vec![fx.int(types::I8, 0)],
                )[0];
                vec![same]
            },
            |_, fx| vec![fx.int(types::I8, 0)],
        )[0];
        let absent = fx.b.ins().bxor_imm_s(present, 1);
        let stop = fx.b.ins().bor(same, absent);
        let next = fx.choose(
            stop,
            &[PTR, PTR],
            |_| vec![link, current],
            |fx| {
                let next_link = fx.offset(current, file::NEXT);
                vec![next_link, fx.load(PTR, current, file::NEXT)]
            },
        );
        (fx.b.ins().bxor_imm_s(stop, 1), next)
    });
    let current = found[1];
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
    let zero = fx.ptr(0);
    let link = fx.b.ins().select(present, found[0], zero);
    (current, link)
}

/// The file of a joined path, created empty when missing; the joined path's memory passes to
/// a created file or is freed.
fn find_or_create(lo: &mut Lowering, fx: &mut Fx, system: Value, path: (Value, Value)) -> Value {
    let (found, _) = find(lo, fx, system, path);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, found, 0);
    fx.choose_in(
        lo,
        missing,
        &[PTR],
        |lo, fx| {
            let size = fx.ptr(file::SIZE);
            let created = lo.record_alloc(fx, size);
            super::exhausted_unless(lo, fx, created);
            fx.store(path.0, created, file::PATH);
            fx.store(path.1, created, file::PATH_LEN);
            let first = fx.load(PTR, system, system::FILES);
            fx.store(first, created, file::NEXT);
            fx.store(created, system, system::FILES);
            vec![created]
        },
        |lo, fx| {
            lo.record_free(fx, path.0);
            vec![found]
        },
    )[0]
}

/// Replaces a file's bytes with `len` bytes, the first `kept` of them its own, then those at
/// `data`; fails with `NoSpace`, changing nothing, when its file system's total size would pass
/// its limit.
fn resize(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    system: Value,
    target: Value,
    kept: Value,
    data: (Value, Value),
) {
    let old = fx.load(PTR, target, file::LEN);
    let len = fx.b.ins().iadd(kept, data.1);
    let total = fx.load(PTR, system, system::TOTAL);
    let total = fx.b.ins().isub(total, old);
    let (total, overflow) = fx.b.ins().uadd_overflow(total, len);
    let limit = fx.load(PTR, system, system::LIMIT);
    let over = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, total, limit);
    let over = fx.b.ins().bor(over, overflow);
    fx.when(over, |fx| sys::fail_with(lo, fx, supply, "NoSpace"));
    let cell = fx.load(PTR, system, system::CELL);
    let bytes = draw(lo, fx, cell, len);
    let previous = fx.load(PTR, target, file::DATA);
    lo.call_routine(fx, Routine::Copy, &[bytes, previous, kept]);
    let tail = fx.b.ins().iadd(bytes, kept);
    lo.call_routine(fx, Routine::Copy, &[tail, data.0, data.1]);
    let held = fx.b.ins().icmp_imm_s(IntCC::NotEqual, previous, 0);
    fx.when(held, |fx| {
        lo.call_routine(fx, Routine::Free, &[previous]);
    });
    fx.store(bytes, target, file::DATA);
    fx.store(len, target, file::LEN);
    fx.store(total, system, system::TOTAL);
}

/// `sys::Fs.open` and `sys::Fs.create`: a file opened for reading, which must exist, or for
/// writing, created or truncated.
pub fn open(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, record: Value, creates: bool) {
    cancellation_point(lo, fx, supply);
    let (system, prefix) = of_entry(lo, fx, record);
    let path = joined(lo, fx, prefix, supply.param(1));
    let target = if creates {
        let target = find_or_create(lo, fx, system, path);
        let none = fx.ptr(0);
        let empty = (none, none);
        resize(lo, fx, supply, system, target, none, empty);
        target
    } else {
        let (target, _) = find(lo, fx, system, path);
        lo.record_free(fx, path.0);
        let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, target, 0);
        fx.when(missing, |fx| sys::fail_with(lo, fx, supply, "NotFound"));
        target
    };
    let (index, opened) = new_object(lo, fx, object::FILE, open::SIZE);
    fx.store(system, opened, open::SYSTEM);
    fx.store(target, opened, open::FILE);
    let writing = fx.ptr(u64::from(creates));
    fx.store(writing, opened, open::WRITING);
    let capability = supply.param(0).address();
    let handle = fx.load(PTR, capability, 0);
    sys::ret_resource(lo, fx, supply, handle, index);
}

/// `sys::Fs.read_all`: a copy of a file's bytes drawn through `alloc`.
pub fn read_all(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, record: Value) {
    cancellation_point(lo, fx, supply);
    let (system, prefix) = of_entry(lo, fx, record);
    let path = joined(lo, fx, prefix, supply.param(1));
    let (target, _) = find(lo, fx, system, path);
    lo.record_free(fx, path.0);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, target, 0);
    fx.when(missing, |fx| sys::fail_with(lo, fx, supply, "NotFound"));
    let alloc = fx.load(PTR, supply.param(2).address(), 0);
    let len = fx.load(PTR, target, file::LEN);
    let bytes = supply.sret.expect("the bytes' address");
    let element = vector::Element { size: 1, align: 1 };
    vector::empty(fx, bytes, alloc, element);
    let any = fx.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
    fx.when(any, |fx| {
        let one = fx.ptr(1);
        let storage = lo.call_routine(fx, Routine::Alloc, &[alloc, len, one])[0];
        super::exhausted_unless(lo, fx, storage);
        let data = fx.load(PTR, target, file::DATA);
        lo.call_routine(fx, Routine::Copy, &[storage, data, len]);
        fx.store(storage, bytes, vector::DATA);
        fx.store(len, bytes, vector::LEN);
        fx.store(len, bytes, vector::CAPACITY);
    });
    supply.ret(lo, fx, Held::Address(bytes));
}

/// `sys::Fs.write_all`: a file holding exactly `data`, created when missing.
pub fn write_all(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, record: Value) {
    cancellation_point(lo, fx, supply);
    let (system, prefix) = of_entry(lo, fx, record);
    let path = joined(lo, fx, prefix, supply.param(1));
    let target = find_or_create(lo, fx, system, path);
    let data = sys::view(fx, supply.param(2));
    let none = fx.ptr(0);
    resize(lo, fx, supply, system, target, none, data);
    supply.done(lo, fx);
}

/// `sys::Fs.remove`: the file leaves its file system, its bytes freed once no open file reads
/// them.
pub fn remove(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, record: Value) {
    cancellation_point(lo, fx, supply);
    let (system, prefix) = of_entry(lo, fx, record);
    let path = joined(lo, fx, prefix, supply.param(1));
    let (target, link) = find(lo, fx, system, path);
    lo.record_free(fx, path.0);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, target, 0);
    fx.when(missing, |fx| sys::fail_with(lo, fx, supply, "NotFound"));
    let next = fx.load(PTR, target, file::NEXT);
    fx.store(next, link, 0);
    let len = fx.load(PTR, target, file::LEN);
    let total = fx.load(PTR, system, system::TOTAL);
    let total = fx.b.ins().isub(total, len);
    fx.store(total, system, system::TOTAL);
    supply.done(lo, fx);
}

/// The open file a resource at an address names.
fn opened(lo: &mut Lowering, fx: &mut Fx, address: Value) -> Value {
    let holding = fx.offset(address, 8);
    let index = super::super::super::runtime::holdings::payload(fx, holding);
    object_at(lo, fx, index)
}

/// `sys::fs::File.read`: the bytes from the position, as many as fit, none at the end; a file
/// opened for writing refuses.
pub fn read(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    cancellation_point(lo, fx, supply);
    let opened = opened(lo, fx, supply.param(0).address());
    let writing = fx.load(PTR, opened, open::WRITING);
    let writing = fx.b.ins().icmp_imm_s(IntCC::NotEqual, writing, 0);
    fx.when(writing, |fx| sys::fail_with(lo, fx, supply, "Denied"));
    let target = fx.load(PTR, opened, open::FILE);
    let position = fx.load(PTR, opened, open::POSITION);
    let len = fx.load(PTR, target, file::LEN);
    let left = fx.b.ins().isub(len, position);
    let (start, room) = sys::view(fx, supply.param(1));
    let count = fx.b.ins().umin(left, room);
    let data = fx.load(PTR, target, file::DATA);
    let from = fx.b.ins().iadd(data, position);
    lo.call_routine(fx, Routine::Copy, &[start, from, count]);
    let position = fx.b.ins().iadd(position, count);
    fx.store(position, opened, open::POSITION);
    supply.ret(lo, fx, Held::Scalar(count));
}

/// `sys::fs::File.write`: all of `data` at the position, extending the file; a file opened for
/// reading refuses.
pub fn write(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    cancellation_point(lo, fx, supply);
    let opened = opened(lo, fx, supply.param(0).address());
    let writing = fx.load(PTR, opened, open::WRITING);
    let reading = fx.b.ins().icmp_imm_s(IntCC::Equal, writing, 0);
    fx.when(reading, |fx| sys::fail_with(lo, fx, supply, "Denied"));
    let system = fx.load(PTR, opened, open::SYSTEM);
    let target = fx.load(PTR, opened, open::FILE);
    let position = fx.load(PTR, opened, open::POSITION);
    let data = sys::view(fx, supply.param(1));
    let len = fx.load(PTR, target, file::LEN);
    let end = fx.b.ins().iadd(position, data.1);
    let beyond = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, len, end);
    fx.choose_in(
        lo,
        beyond,
        &[],
        |lo, fx| {
            let bytes = fx.load(PTR, target, file::DATA);
            let at = fx.b.ins().iadd(bytes, position);
            lo.call_routine(fx, Routine::Copy, &[at, data.0, data.1]);
            Vec::new()
        },
        |lo, fx| {
            resize(lo, fx, supply, system, target, position, data);
            Vec::new()
        },
    );
    fx.store(end, opened, open::POSITION);
    supply.ret(lo, fx, Held::Scalar(data.1));
}

/// `sys::fs::File.byte_len`: the file's size.
pub fn byte_len(lo: &mut Lowering, fx: &mut Fx, supply: &Supply) {
    cancellation_point(lo, fx, supply);
    let opened = opened(lo, fx, supply.param(0).address());
    let target = fx.load(PTR, opened, open::FILE);
    let len = fx.load(PTR, target, file::LEN);
    supply.ret(lo, fx, Held::Scalar(len));
}
