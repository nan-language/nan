//! The simulated file system: its files and directories in byte order of joined path, each file
//! holding bytes drawn through the file system's allocator and counted against its size limit,
//! and its open files, each the file it reads or writes and its position.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::super::Lowering;
use super::super::super::body::Held;
use super::super::super::fx::{Fx, PTR};
use super::super::super::runtime::{Routine, report};
use super::super::Supply;
use super::super::mem::vector;
use super::super::sys;
use super::{cancellation_point, draw, new_object, object, object_at};
use crate::check::ty::Ty;

/// The offsets of a file system's fields: its size limit, its files' total size, the cell it
/// draws through and its first file.
mod system {
    pub const LIMIT: u64 = 8;
    pub const TOTAL: u64 = 16;
    pub const CELL: u64 = 24;
    pub const FILES: u64 = 32;
    pub const SIZE: u64 = 40;
}

/// The offsets of a file's fields: the next file, its joined path, its bytes and whether it is
/// a directory, which holds no bytes.
mod file {
    pub const NEXT: u64 = 0;
    pub const PATH: u64 = 8;
    pub const PATH_LEN: u64 = 16;
    pub const DATA: u64 = 24;
    pub const LEN: u64 = 32;
    pub const DIRECTORY: u64 = 40;
    pub const SIZE: u64 = 48;
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

/// The file system at `index`, and the joined path of the prefix of the operation's `Fs` and
/// its path, failing the operation with `Revoked.Stale` when that `Fs` is stale.
fn resolved(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    index: Value,
) -> (Value, (Value, Value)) {
    let system = object_at(lo, fx, index);
    let prefix = sys::checked_text(lo, fx, supply, supply.param(0).address());
    let path = joined(lo, fx, prefix, supply.param(1));
    lo.record_free(fx, prefix.0);
    (system, path)
}

/// The joined path of a prefix and a path in runtime memory: the prefix, `/` when neither is
/// empty, and the path.
fn joined(lo: &mut Lowering, fx: &mut Fx, prefix: (Value, Value), path: Held) -> (Value, Value) {
    let (path_start, path_len) = sys::view(fx, path);
    let has_prefix = fx.b.ins().icmp_imm_s(IntCC::NotEqual, prefix.1, 0);
    let has_path = fx.b.ins().icmp_imm_s(IntCC::NotEqual, path_len, 0);
    let separated = fx.b.ins().band(has_prefix, has_path);
    let separator = fx.b.ins().uextend(PTR, separated);
    let len = fx.b.ins().iadd(prefix.1, separator);
    let len = fx.b.ins().iadd(len, path_len);
    let size = fx.b.ins().iadd_imm_s(len, 1);
    let text = lo.record_alloc(fx, size);
    super::exhausted_unless(lo, fx, text);
    lo.call_routine(fx, Routine::Copy, &[text, prefix.0, prefix.1]);
    fx.when(separated, |fx| {
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
            let file = fx.ptr(0);
            vec![create(lo, fx, system, path, file)]
        },
        |lo, fx| {
            lo.record_free(fx, path.0);
            vec![found]
        },
    )[0]
}

/// A new empty file, or a directory when `directory` is 1, of a joined path, linked among its
/// file system's files in byte order of path; the joined path's memory passes to it.
fn create(
    lo: &mut Lowering,
    fx: &mut Fx,
    system: Value,
    path: (Value, Value),
    directory: Value,
) -> Value {
    let size = fx.ptr(file::SIZE);
    let created = lo.record_alloc(fx, size);
    super::exhausted_unless(lo, fx, created);
    fx.store(path.0, created, file::PATH);
    fx.store(path.1, created, file::PATH_LEN);
    fx.store(directory, created, file::DIRECTORY);
    insert(lo, fx, system, created);
    created
}

/// Links a file before the first of its file system's files whose path comes after its own.
fn insert(lo: &mut Lowering, fx: &mut Fx, system: Value, record: Value) {
    let path = path_of(fx, record);
    let link = fx.offset(system, system::FILES);
    let first = fx.load(PTR, link, 0);
    let found = fx.repeat(&[link, first], |fx, state| {
        let (link, current) = (state[0], state[1]);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let later = fx.choose_in(
            lo,
            present,
            &[types::I8],
            |lo, fx| {
                let other = path_of(fx, current);
                vec![comes_after(lo, fx, other, path)]
            },
            |_, fx| vec![fx.int(types::I8, 0)],
        )[0];
        let absent = fx.b.ins().bxor_imm_s(present, 1);
        let stop = fx.b.ins().bor(later, absent);
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
    fx.store(found[1], record, file::NEXT);
    fx.store(record, found[0], 0);
}

/// The joined path of a file.
fn path_of(fx: &mut Fx, record: Value) -> (Value, Value) {
    (
        fx.load(PTR, record, file::PATH),
        fx.load(PTR, record, file::PATH_LEN),
    )
}

/// Whether the path `a` comes after the path `b` in byte order.
fn comes_after(lo: &mut Lowering, fx: &mut Fx, a: (Value, Value), b: (Value, Value)) -> Value {
    let shorter = fx.b.ins().umin(a.1, b.1);
    let order = lo.call_routine(fx, Routine::Compare, &[a.0, b.0, shorter])[0];
    let greater = fx.b.ins().icmp_imm_s(IntCC::SignedGreaterThan, order, 0);
    let equal = fx.b.ins().icmp_imm_s(IntCC::Equal, order, 0);
    let longer = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, a.1, b.1);
    let tie = fx.b.ins().band(equal, longer);
    fx.b.ins().bor(greater, tie)
}

/// Whether the path `own` is `path` or lies below it.
fn under(lo: &mut Lowering, fx: &mut Fx, own: (Value, Value), path: (Value, Value)) -> Value {
    let long =
        fx.b.ins()
            .icmp(IntCC::UnsignedGreaterThanOrEqual, own.1, path.1);
    fx.choose_in(
        lo,
        long,
        &[types::I8],
        |lo, fx| {
            let order = lo.call_routine(fx, Routine::Compare, &[own.0, path.0, path.1])[0];
            let prefixed = fx.b.ins().icmp_imm_s(IntCC::Equal, order, 0);
            let exact = fx.b.ins().icmp(IntCC::Equal, own.1, path.1);
            let parted = fx.choose(
                exact,
                &[types::I8],
                |fx| vec![fx.int(types::I8, 0)],
                |fx| {
                    let at = fx.b.ins().iadd(own.0, path.1);
                    let byte = fx.load(types::I8, at, 0);
                    vec![fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'/'))]
                },
            )[0];
            let below = fx.b.ins().bor(exact, parted);
            vec![fx.b.ins().band(prefixed, below)]
        },
        |_, fx| vec![fx.int(types::I8, 0)],
    )[0]
}

/// Whether the path `own` lies below `path`.
fn strictly_under(
    lo: &mut Lowering,
    fx: &mut Fx,
    own: (Value, Value),
    path: (Value, Value),
) -> Value {
    let under = under(lo, fx, own, path);
    let other = fx.b.ins().icmp(IntCC::NotEqual, own.1, path.1);
    fx.b.ins().band(under, other)
}

/// Whether `len` bytes from `start` hold a `/`.
fn holds_slash(fx: &mut Fx, (start, len): (Value, Value)) -> Value {
    let zero = fx.ptr(0);
    let none = fx.int(types::I8, 0);
    fx.repeat(&[zero, none], |fx, state| {
        let (index, found) = (state[0], state[1]);
        let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, len);
        let found = fx.choose(
            inside,
            &[types::I8],
            |fx| {
                let at = fx.b.ins().iadd(start, index);
                let byte = fx.load(types::I8, at, 0);
                vec![fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'/'))]
            },
            |_| vec![found],
        )[0];
        let absent = fx.b.ins().bxor_imm_s(found, 1);
        let going = fx.b.ins().band(inside, absent);
        let next = fx.b.ins().iadd_imm_s(index, 1);
        (going, vec![next, found])
    })[1]
}

/// The directories a joined path passes through, and the path itself when `through`, made
/// where missing; a file on the way fails the operation with `Denied`.
fn make_directories(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    system: Value,
    path: (Value, Value),
    through: bool,
) {
    let zero = fx.ptr(0);
    fx.repeat(&[zero], |fx, state| {
        let index = state[0];
        let inside = fx.b.ins().icmp(IntCC::UnsignedLessThan, index, path.1);
        fx.when(inside, |fx| {
            let at = fx.b.ins().iadd(path.0, index);
            let byte = fx.load(types::I8, at, 0);
            let slash = fx.b.ins().icmp_imm_s(IntCC::Equal, byte, i64::from(b'/'));
            fx.when(slash, |fx| {
                directory_at(lo, fx, supply, system, (path.0, index))
            });
        });
        (inside, vec![fx.b.ins().iadd_imm_s(index, 1)])
    });
    if through {
        directory_at(lo, fx, supply, system, path);
    }
}

/// The directory of the joined path of `len` bytes from `start`, made of a copy of them when
/// missing; a file there fails the operation with `Denied`.
fn directory_at(
    lo: &mut Lowering,
    fx: &mut Fx,
    supply: &Supply,
    system: Value,
    (start, len): (Value, Value),
) {
    let (found, _) = find(lo, fx, system, (start, len));
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, found, 0);
    fx.choose_in(
        lo,
        missing,
        &[],
        |lo, fx| {
            let size = fx.b.ins().iadd_imm_s(len, 1);
            let text = lo.record_alloc(fx, size);
            super::exhausted_unless(lo, fx, text);
            lo.call_routine(fx, Routine::Copy, &[text, start, len]);
            let directory = fx.ptr(1);
            create(lo, fx, system, (text, len), directory);
            Vec::new()
        },
        |lo, fx| {
            refuse_unless_directory(lo, fx, supply, found);
            Vec::new()
        },
    );
}

/// Fails the operation with `Denied` when a file of the file system is a directory.
fn refuse_directory(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, record: Value) {
    let directory = fx.load(PTR, record, file::DIRECTORY);
    let refused = fx.b.ins().icmp_imm_s(IntCC::NotEqual, directory, 0);
    fx.when(refused, |fx| sys::fail_with(lo, fx, supply, "Denied"));
}

/// Fails the operation with `Denied` when a file of the file system is not a directory.
fn refuse_unless_directory(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, record: Value) {
    let directory = fx.load(PTR, record, file::DIRECTORY);
    let refused = fx.b.ins().icmp_imm_s(IntCC::Equal, directory, 0);
    fx.when(refused, |fx| sys::fail_with(lo, fx, supply, "Denied"));
}

/// Unlinks a file from its file system, its bytes no longer counted against its size limit.
fn unlink(fx: &mut Fx, system: Value, record: Value, link: Value) {
    let next = fx.load(PTR, record, file::NEXT);
    fx.store(next, link, 0);
    let len = fx.load(PTR, record, file::LEN);
    let total = fx.load(PTR, system, system::TOTAL);
    let total = fx.b.ins().isub(total, len);
    fx.store(total, system, system::TOTAL);
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
pub fn open(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, index: Value, creates: bool) {
    cancellation_point(lo, fx, supply);
    let (system, path) = resolved(lo, fx, supply, index);
    let target = if creates {
        make_directories(lo, fx, supply, system, path, false);
        let target = find_or_create(lo, fx, system, path);
        refuse_directory(lo, fx, supply, target);
        let none = fx.ptr(0);
        let empty = (none, none);
        resize(lo, fx, supply, system, target, none, empty);
        target
    } else {
        let (target, _) = find(lo, fx, system, path);
        lo.record_free(fx, path.0);
        let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, target, 0);
        fx.when(missing, |fx| sys::fail_with(lo, fx, supply, "NotFound"));
        refuse_directory(lo, fx, supply, target);
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
pub fn read_all(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, index: Value) {
    cancellation_point(lo, fx, supply);
    let (system, path) = resolved(lo, fx, supply, index);
    let (target, _) = find(lo, fx, system, path);
    lo.record_free(fx, path.0);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, target, 0);
    fx.when(missing, |fx| sys::fail_with(lo, fx, supply, "NotFound"));
    refuse_directory(lo, fx, supply, target);
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
pub fn write_all(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, index: Value) {
    cancellation_point(lo, fx, supply);
    let (system, path) = resolved(lo, fx, supply, index);
    make_directories(lo, fx, supply, system, path, false);
    let target = find_or_create(lo, fx, system, path);
    refuse_directory(lo, fx, supply, target);
    let data = sys::view(fx, supply.param(2));
    let none = fx.ptr(0);
    resize(lo, fx, supply, system, target, none, data);
    supply.done(lo, fx);
}

/// `sys::Fs.remove`: the file or empty directory leaves its file system, a file's bytes freed
/// once no open file reads them.
pub fn remove(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, index: Value) {
    cancellation_point(lo, fx, supply);
    let (system, path) = resolved(lo, fx, supply, index);
    let (target, link) = find(lo, fx, system, path);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, target, 0);
    fx.when(missing, |fx| {
        lo.record_free(fx, path.0);
        sys::fail_with(lo, fx, supply, "NotFound");
    });
    let occupied = holds_below(lo, fx, system, path);
    lo.record_free(fx, path.0);
    fx.when(occupied, |fx| sys::fail_with(lo, fx, supply, "Denied"));
    unlink(fx, system, target, link);
    supply.done(lo, fx);
}

/// Whether a file of the file system lies below a joined path.
fn holds_below(lo: &mut Lowering, fx: &mut Fx, system: Value, path: (Value, Value)) -> Value {
    let first = fx.load(PTR, system, system::FILES);
    let none = fx.int(types::I8, 0);
    fx.repeat(&[first, none], |fx, state| {
        let (current, _) = (state[0], state[1]);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let below = fx.choose_in(
            lo,
            present,
            &[types::I8],
            |lo, fx| {
                let own = path_of(fx, current);
                vec![strictly_under(lo, fx, own, path)]
            },
            |_, fx| vec![fx.int(types::I8, 0)],
        )[0];
        let next = fx.choose(
            present,
            &[PTR],
            |fx| vec![fx.load(PTR, current, file::NEXT)],
            |fx| vec![fx.ptr(0)],
        )[0];
        let absent = fx.b.ins().bxor_imm_s(below, 1);
        let going = fx.b.ins().band(present, absent);
        (going, vec![next, below])
    })[1]
}

/// `sys::Fs.create_dir`: the directory and those its path passes through, made where missing.
pub fn create_dir(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, index: Value) {
    cancellation_point(lo, fx, supply);
    let (system, path) = resolved(lo, fx, supply, index);
    make_directories(lo, fx, supply, system, path, true);
    lo.record_free(fx, path.0);
    supply.done(lo, fx);
}

/// `sys::Fs.rename`: the file or directory `from` names, with every file below it, moved to
/// `to`, replacing a file there.
pub fn rename(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, index: Value) {
    cancellation_point(lo, fx, supply);
    let system = object_at(lo, fx, index);
    let prefix = sys::checked_text(lo, fx, supply, supply.param(0).address());
    let from = joined(lo, fx, prefix, supply.param(1));
    let to = joined(lo, fx, prefix, supply.param(2));
    lo.record_free(fx, prefix.0);
    let (moved, _) = find(lo, fx, system, from);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, moved, 0);
    let inside = strictly_under(lo, fx, to, from);
    let refused = fx.b.ins().bor(missing, inside);
    fx.when(refused, |fx| {
        lo.record_free(fx, from.0);
        lo.record_free(fx, to.0);
        fx.when(missing, |fx| sys::fail_with(lo, fx, supply, "NotFound"));
        sys::fail_with(lo, fx, supply, "Denied");
    });
    let same = under(lo, fx, to, from);
    let distinct = fx.b.ins().bxor_imm_s(same, 1);
    fx.when(distinct, |fx| {
        make_directories(lo, fx, supply, system, to, false);
        let (target, link) = find(lo, fx, system, to);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, target, 0);
        fx.when(present, |fx| {
            refuse_directory(lo, fx, supply, target);
            refuse_directory(lo, fx, supply, moved);
            unlink(fx, system, target, link);
        });
        let chain = gather(lo, fx, system, from);
        fx.repeat(&[chain], |fx, state| {
            let current = state[0];
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
            let next = fx.choose(
                present,
                &[PTR],
                |fx| vec![fx.load(PTR, current, file::NEXT)],
                |fx| vec![fx.ptr(0)],
            )[0];
            fx.when(present, |fx| {
                let old = path_of(fx, current);
                let tail = fx.b.ins().isub(old.1, from.1);
                let len = fx.b.ins().iadd(to.1, tail);
                let size = fx.b.ins().iadd_imm_s(len, 1);
                let text = lo.record_alloc(fx, size);
                super::exhausted_unless(lo, fx, text);
                lo.call_routine(fx, Routine::Copy, &[text, to.0, to.1]);
                let at = fx.b.ins().iadd(text, to.1);
                let rest = fx.b.ins().iadd(old.0, from.1);
                lo.call_routine(fx, Routine::Copy, &[at, rest, tail]);
                lo.record_free(fx, old.0);
                fx.store(text, current, file::PATH);
                fx.store(len, current, file::PATH_LEN);
                insert(lo, fx, system, current);
            });
            (present, vec![next])
        });
    });
    lo.record_free(fx, from.0);
    lo.record_free(fx, to.0);
    supply.done(lo, fx);
}

/// Unlinks from a file system the file of a joined path and every file below it, giving them
/// as a chain through their links.
fn gather(lo: &mut Lowering, fx: &mut Fx, system: Value, path: (Value, Value)) -> Value {
    let link = fx.offset(system, system::FILES);
    let zero = fx.ptr(0);
    fx.repeat(&[link, zero], |fx, state| {
        let (link, chain) = (state[0], state[1]);
        let current = fx.load(PTR, link, 0);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let next = fx.choose_in(
            lo,
            present,
            &[PTR, PTR],
            |lo, fx| {
                let own = path_of(fx, current);
                let caught = under(lo, fx, own, path);
                fx.choose(
                    caught,
                    &[PTR, PTR],
                    |fx| {
                        let after = fx.load(PTR, current, file::NEXT);
                        fx.store(after, link, 0);
                        fx.store(chain, current, file::NEXT);
                        vec![link, current]
                    },
                    |fx| vec![fx.offset(current, file::NEXT), chain],
                )
            },
            |_, _| vec![link, chain],
        );
        (present, next)
    })[1]
}

/// `sys::Fs.kind`: a directory for the empty path or a directory, a file for a file.
pub fn kind(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, index: Value) {
    cancellation_point(lo, fx, supply);
    let (system, path) = resolved(lo, fx, supply, index);
    let (found, _) = find(lo, fx, system, path);
    lo.record_free(fx, path.0);
    let root = fx.b.ins().icmp_imm_s(IntCC::Equal, path.1, 0);
    let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, found, 0);
    let named = fx.b.ins().bxor_imm_s(root, 1);
    let absent = fx.b.ins().band(missing, named);
    fx.when(absent, |fx| sys::fail_with(lo, fx, supply, "NotFound"));
    let directory = fx.choose(
        root,
        &[types::I8],
        |fx| vec![fx.int(types::I8, 1)],
        |fx| {
            let directory = fx.load(PTR, found, file::DIRECTORY);
            vec![fx.b.ins().icmp_imm_s(IntCC::NotEqual, directory, 0)]
        },
    )[0];
    let kind = supply.abi.result.clone();
    let layout = lo.layout(&kind);
    let slot = fx.slot(layout.size, layout.align);
    write_kind(lo, fx, &kind, slot, directory);
    let held = lo.held_at(fx, &kind, slot);
    supply.ret(lo, fx, held);
}

/// Writes at `at` the `sys::fs::Kind` of a file or, when `directory` holds, a directory.
fn write_kind(lo: &mut Lowering, fx: &mut Fx, kind: &Ty, at: Value, directory: Value) {
    fx.choose_in(
        lo,
        directory,
        &[],
        |lo, fx| {
            let folder = lo.variant_value(fx, kind, "Directory", &[], None);
            lo.store_held(fx, at, folder, kind);
            Vec::new()
        },
        |lo, fx| {
            let file = lo.variant_value(fx, kind, "File", &[], None);
            lo.store_held(fx, at, file, kind);
            Vec::new()
        },
    );
}

/// `sys::Fs.list`: the entries of the directory, the root for the empty path, in byte order of
/// name, drawn through `alloc`.
pub fn list(lo: &mut Lowering, fx: &mut Fx, supply: &Supply, index: Value) {
    cancellation_point(lo, fx, supply);
    let (system, path) = resolved(lo, fx, supply, index);
    let root = fx.b.ins().icmp_imm_s(IntCC::Equal, path.1, 0);
    let named = fx.b.ins().bxor_imm_s(root, 1);
    fx.when(named, |fx| {
        let (found, _) = find(lo, fx, system, path);
        let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, found, 0);
        fx.when(missing, |fx| {
            lo.record_free(fx, path.0);
            sys::fail_with(lo, fx, supply, "NotFound");
        });
        let directory = fx.load(PTR, found, file::DIRECTORY);
        let file = fx.b.ins().icmp_imm_s(IntCC::Equal, directory, 0);
        fx.when(file, |fx| {
            lo.record_free(fx, path.0);
            sys::fail_with(lo, fx, supply, "Denied");
        });
    });
    let alloc = fx.load(PTR, supply.param(2).address(), 0);
    let entries = supply.sret.expect("the entries' address");
    let entry = supply
        .abi
        .result
        .type_arg(0)
        .cloned()
        .expect("the entry type");
    let element = vector::Element::of(lo, &entry);
    vector::empty(fx, entries, alloc, element);
    let start = fx.b.ins().iadd_imm_s(path.1, 1);
    let zero = fx.ptr(0);
    let start = fx.b.ins().select(root, zero, start);
    let first = fx.load(PTR, system, system::FILES);
    fx.repeat(&[first], |fx, state| {
        let current = state[0];
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
        let next = fx.choose(
            present,
            &[PTR],
            |fx| vec![fx.load(PTR, current, file::NEXT)],
            |fx| vec![fx.ptr(0)],
        )[0];
        fx.when(present, |fx| {
            let own = path_of(fx, current);
            let below = fx.choose_in(
                lo,
                root,
                &[types::I8],
                |_, fx| vec![fx.int(types::I8, 1)],
                |lo, fx| vec![strictly_under(lo, fx, own, path)],
            )[0];
            fx.when(below, |fx| {
                let name = fx.b.ins().iadd(own.0, start);
                let len = fx.b.ins().isub(own.1, start);
                let nested = holds_slash(fx, (name, len));
                let child = fx.b.ins().bxor_imm_s(nested, 1);
                fx.when(child, |fx| {
                    push_entry(lo, fx, (entries, alloc), &entry, current, (name, len));
                });
            });
        });
        (present, vec![next])
    });
    lo.record_free(fx, path.0);
    supply.ret(lo, fx, Held::Address(entries));
}

/// Appends to the list at `entries` drawing through `alloc` the entry of a file named by `len`
/// bytes from `name`: a copy of them and the file's kind.
fn push_entry(
    lo: &mut Lowering,
    fx: &mut Fx,
    (entries, alloc): (Value, Value),
    entry: &Ty,
    record: Value,
    (name, len): (Value, Value),
) {
    let one = fx.ptr(1);
    let text = lo.call_routine(fx, Routine::Alloc, &[alloc, len, one])[0];
    super::exhausted_unless(lo, fx, text);
    lo.call_routine(fx, Routine::Copy, &[text, name, len]);
    let element = vector::Element::of(lo, entry);
    let count = fx.load(PTR, entries, vector::LEN);
    let needed = fx.b.ins().iadd_imm_s(count, 1);
    let room = vector::reserve(lo, fx, entries, needed, element);
    let short = fx.b.ins().icmp_imm_s(IntCC::Equal, room, 0);
    fx.when(short, |fx| report::raise_kind(lo, fx, "trap.exhausted"));
    let data = fx.load(PTR, entries, vector::DATA);
    let at = element.at(fx, data, count);
    let name_at = lo.layouts.field(lo.ck, entry, None, "name").offset;
    let kind_field = lo.layouts.field(lo.ck, entry, None, "kind");
    let string = fx.offset(at, name_at);
    fx.store(text, string, vector::DATA);
    fx.store(len, string, vector::LEN);
    fx.store(len, string, vector::CAPACITY);
    fx.store(alloc, string, vector::ALLOC);
    let directory = fx.load(PTR, record, file::DIRECTORY);
    let directory = fx.b.ins().icmp_imm_s(IntCC::NotEqual, directory, 0);
    let kind_at = fx.offset(at, kind_field.offset);
    write_kind(lo, fx, &kind_field.ty, kind_at, directory);
    fx.store(needed, entries, vector::LEN);
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
