//! The capabilities interpreter runs see: `testing::world()` and the sims, operated without
//! waiting; the random streams of the seeded scheduler, the virtual clock and the virtual wall
//! clock reading it, the path checks of
//! file systems, attenuations, logs, whose test trace example runs discard, and revocation.
//!
//! A capability holds a reference to its capability table entry, which records whether it is
//! live and the entry it derives from; a random stream also holds its seed and next byte.

use std::rc::Rc;

use super::super::value::{Loc, Val};
use super::super::{Exec, Flow, Interpreter};
use super::stores::{WORLD_LIMIT, allocator, cell_of, container, place};
use crate::check::sig::SigKind;
use crate::check::ty::{Arg, Ty};
use crate::hash::blake3;

/// The stream of the seeded scheduler serving random bytes.
const RANDOM_STREAM: u64 = 2;

/// The positions of a capability's parts: its entry, then a random stream's seed and the index
/// of its next byte.
pub(super) const ENTRY: usize = 0;
const SEED: usize = 1;
const NEXT: usize = 2;

/// The positions of an entry's parts: whether it is live and the entry it derives from.
pub(super) const LIVE: usize = 0;
pub(super) const SOURCE: usize = 1;

/// The positions of a revoker's parts: the capability and what revoking it ends, its entry or
/// an allocator's cell.
const REVOKED: usize = 0;
const HANDLE: usize = 1;

/// A capability operation an interpreter run can make, or `testing::world()`; none for another.
pub fn member(
    interp: &mut Interpreter,
    owner: &str,
    member: &str,
    type_args: &[Arg],
    args: &[Val],
) -> Option<Exec<Flow>> {
    let stale = |interp: &Interpreter| stale(interp, &args[0]);
    let value = match (owner, member) {
        ("testing::world", "") => world(interp),
        ("sys::sim::random", "") => random(interp, args[0].int() as u64),
        ("sys::sim::fs" | "sys::sim::net" | "sys::sim::stdio", "") => {
            let name = match owner {
                "sys::sim::fs" => "sys::Fs",
                "sys::sim::net" => "sys::Net",
                _ => "sys::Stdio",
            };
            capability(interp, name)
        }
        ("sys::Random", "fill") => {
            if stale(interp) {
                return Some(Ok(Flow::Fail(revoked(interp))));
            }
            let random = interp.deref(&args[0]);
            let seed = random.fields()[SEED].int() as u64;
            let next = place(&random.fields()[NEXT]);
            let view = interp.view(&args[1]);
            let start = interp.memory.get(&next).int() as u64;
            for offset in 0..view.len as u64 {
                let index = start + offset;
                let value = stream_value(seed, RANDOM_STREAM, index / 8);
                let byte = (value >> (8 * (index % 8))) & 0xFF;
                let element = view.array.with((view.start as u64 + offset) as u32);
                interp.memory.set(&element, Val::Int(i128::from(byte)));
            }
            interp
                .memory
                .set(&next, Val::Int(i128::from(start + view.len as u64)));
            Val::Unit
        }
        ("sys::Clock", "elapsed_ms" | "elapsed_ns") | ("sys::WallClock", "now_ns") => {
            match stale(interp) {
                true => return Some(Ok(Flow::Fail(revoked(interp)))),
                false => Val::Int(0),
            }
        }
        ("sys::Fs", "subdir") => {
            if stale(interp) {
                let error = interp.ty_item("sys::fs", "FsError");
                let index = interp.variant_index(error, "Revoked");
                let payload = Rc::new(vec![revoked(interp)]);
                return Some(Ok(Flow::Fail(Val::Variant(error, index, payload))));
            }
            let path = interp.bytes(&args[1]);
            let valid = path
                .split(|byte| *byte == b'/')
                .all(|segment| !matches!(segment, b"" | b"." | b"..") && !segment.contains(&0));
            if !valid {
                let error = interp.ty_item("sys::fs", "FsError");
                let index = interp.variant_index(error, "InvalidPath");
                let failure = Val::Variant(error, index, Rc::new(Vec::new()));
                return Some(Ok(Flow::Fail(failure)));
            }
            derived(interp, &args[0])
        }
        ("sys::Fs", "read_only")
        | ("sys::Net", "only_host")
        | ("sys::Clock" | "sys::WallClock" | "task::Executor", "coarse") => match stale(interp) {
            true => return Some(Ok(Flow::Fail(revoked(interp)))),
            false => derived(interp, &args[0]),
        },
        ("sys::Log", "info" | "warn" | "error") => Val::Unit,
        ("sys::revocable", "") => {
            let alloc = type_args.first().and_then(Arg::ty).is_some_and(|ty| {
                matches!(ty, Ty::Item(item, _) if interp.ck.db.canonical(*item) == "mem::Alloc")
            });
            let (capability, handle) = match alloc {
                true => {
                    let source = cell_of(interp, &args[0]);
                    let cell = interp.budgets.split(source, u128::from(u64::MAX));
                    (allocator(interp, cell), Val::Int(i128::from(cell)))
                }
                false if stale(interp) => return Some(Ok(Flow::Fail(revoked(interp)))),
                false => {
                    let capability = derived(interp, &args[0]);
                    let entry = capability.fields()[ENTRY].clone();
                    (capability, entry)
                }
            };
            let item = interp.ty_item("sys", "Revoker");
            Val::Struct(item, Rc::new(vec![capability, handle]))
        }
        ("sys::Revoker", "capability") => interp.deref(&args[0]).fields()[REVOKED].clone(),
        ("sys::Revoker", "revoke") => {
            match interp.deref(&args[0]).fields()[HANDLE].clone() {
                Val::Ref(entry) => interp
                    .memory
                    .set(&entry.with(LIVE as u32), Val::Bool(false)),
                cell => interp.budgets.close(cell.int() as u32),
            }
            Val::Unit
        }
        _ => return None,
    };
    Some(Ok(Flow::Return(value)))
}

/// The world `testing::world()` returns: its allocator names a new root cell, its random
/// stream is that of seed 0, its arguments and environment are empty, and each other
/// capability has an entry of its own.
fn world(interp: &mut Interpreter) -> Val {
    let world = interp.ty_item("host", "World");
    let SigKind::Struct { fields } = &interp.ck.sig(world).kind else {
        panic!("host::World is a struct");
    };
    let fields = fields.clone();
    let cell = interp.budgets.root(WORLD_LIMIT);
    let values = fields
        .iter()
        .map(|field| {
            let Ty::Item(item, args) = &field.ty else {
                panic!("a world field of a std type");
            };
            match interp.ck.db.canonical(*item).as_str() {
                "mem::Alloc" => allocator(interp, cell),
                "sys::Random" => random(interp, 0),
                "core::Untrusted" => {
                    let contents = match args.first().and_then(Arg::ty) {
                        Some(Ty::Item(contents, _)) => interp.ck.db.canonical(*contents),
                        other => panic!("Untrusted contents of {other:?}"),
                    };
                    let empty = container(interp, &contents, Vec::new(), cell);
                    Val::Struct(*item, Rc::new(vec![empty]))
                }
                path => capability(interp, path),
            }
        })
        .collect();
    Val::Struct(world, Rc::new(values))
}

/// A capability of the std type at `path` with a new entry.
fn capability(interp: &mut Interpreter, path: &str) -> Val {
    let (module, name) = path.rsplit_once("::").expect("a std path");
    let item = interp.ty_item(module, name);
    Val::Struct(item, Rc::new(vec![entry(interp, None)]))
}

/// A capability like `source` with a new entry deriving from its entry.
fn derived(interp: &mut Interpreter, source: &Val) -> Val {
    let Val::Struct(item, fields) = interp.deref(source) else {
        panic!("a capability value");
    };
    let mut fields = fields.as_ref().clone();
    fields[ENTRY] = entry(interp, Some(place(&fields[ENTRY])));
    Val::Struct(item, Rc::new(fields))
}

/// A new live entry, deriving from `source` when given.
fn entry(interp: &mut Interpreter, source: Option<Loc>) -> Val {
    let source = source.map_or(Val::Absent, Val::Ref);
    let parts = Val::Array(Rc::new(vec![Val::Bool(true), source]));
    let extent = interp.memory.alloc(parts, true, 0);
    Val::Ref(Loc {
        extent,
        path: Vec::new(),
    })
}

/// Whether a capability's entry, or one it derives from, is revoked.
pub(super) fn stale(interp: &Interpreter, capability: &Val) -> bool {
    let mut entry = place(&interp.deref(capability).fields()[ENTRY]);
    loop {
        if !interp.memory.get(&entry.with(LIVE as u32)).bool() {
            return true;
        }
        match interp.memory.get(&entry.with(SOURCE as u32)) {
            Val::Ref(source) => entry = source.clone(),
            _ => return false,
        }
    }
}

/// `sys::Revoked.Stale`.
pub(super) fn revoked(interp: &Interpreter) -> Val {
    Val::Variant(interp.ty_item("sys", "Revoked"), 0, Rc::new(Vec::new()))
}

/// A `sys::Random` yielding the random bytes of a seed, with a new entry.
fn random(interp: &mut Interpreter, seed: u64) -> Val {
    let entry = entry(interp, None);
    let next = interp.memory.alloc(Val::Int(0), true, 0);
    let item = interp.ty_item("sys", "Random");
    Val::Struct(
        item,
        Rc::new(vec![
            entry,
            Val::Int(i128::from(seed)),
            Val::Ref(Loc {
                extent: next,
                path: Vec::new(),
            }),
        ]),
    )
}

/// Value `index` of stream `stream` of a seed.
fn stream_value(seed: u64, stream: u64, index: u64) -> u64 {
    let mut input = Vec::with_capacity(24);
    for part in [seed, stream, index] {
        input.extend_from_slice(&part.to_le_bytes());
    }
    let hash = blake3(&input);
    u64::from_le_bytes(hash[..8].try_into().expect("eight bytes"))
}
