//! Release walks: the release of the types of a **group**, those whose releases reach each
//! other through the values boxes, shared values and vectors hold. One function per group walks
//! the values it releases depth first without calling itself, keeping its place in the values
//! it releases.
//!
//! A **frame** is a value of a group type being released, and a **holder** a box, shared value,
//! list, set, map or hash map it holds, inline, of a group type. Entering a frame releases its
//! other parts and frees the storage of its empty vectors. The frame then enters, in turn, what
//! each holder still holds, a vector's last element first, after storing in the holder the
//! **link** naming the frame's own parent: its address in the low 49 bits, its type's index in
//! the group above. Once a frame holds nothing more, its parent finds the first holder still
//! holding something, which is the one it entered the frame from, reads the link back from it,
//! takes the frame out, freeing a box's or shared value's storage or a vector's once empty, and
//! goes on. A box's or shared value's word is the link while its value is released and `DONE`
//! once it is.

use std::collections::HashMap;
use std::rc::Rc;

use cranelift_codegen::ir::{
    AbiParam, AtomicRmwOp, Block, InstBuilder, MemFlagsData, Signature, TrapCode, Value,
    condcodes::IntCC,
};

use super::super::fx::{Fx, PTR};
use super::super::layout::Shape;
use super::super::runtime::Routine;
use super::super::supplied::mem::{maps, stores, vector};
use super::super::{CALL_CONV, Glue, Lowering, supplied};
use crate::check::ty::Ty;

/// A box's or shared value's word once the value it holds is released.
const DONE: i64 = 1;

/// The bits of a link holding the frame's address.
const ADDRESS_BITS: i64 = 49;

/// How a holder holds values of a group type.
enum Holder {
    /// The elements of a vector, `indexed` for a hash map's, whose index is freed first.
    Vector { element: Ty, indexed: bool },
    /// The contents of a box.
    Boxed(Ty),
    /// The contents of a shared value, released by its last holder.
    Shared(Ty),
}

impl Holder {
    fn of(lo: &mut Lowering, ty: &Ty) -> Option<Holder> {
        let Ty::Item(item, _) = ty else {
            return None;
        };
        if !lo.ck.db.is_std(*item) {
            return None;
        }
        let arg = |index: usize| ty.type_arg(index).cloned().expect("a type argument");
        Some(match lo.ck.db.canonical(*item).as_str() {
            "mem::List" | "mem::Set" => Holder::Vector {
                element: arg(0),
                indexed: false,
            },
            "mem::Map" | "mem::HashMap" => Holder::Vector {
                element: lo.std_ty("mem", "KeyValue", vec![arg(0), arg(1)]),
                indexed: lo.ck.db.canonical(*item) == "mem::HashMap",
            },
            "mem::Box" if !matches!(ty.type_arg(0), Some(Ty::Dyn(_))) => Holder::Boxed(arg(0)),
            "mem::Shared" => Holder::Shared(arg(0)),
            _ => return None,
        })
    }

    fn held(&self) -> &Ty {
        match self {
            Holder::Vector { element, .. } => element,
            Holder::Boxed(held) | Holder::Shared(held) => held,
        }
    }
}

impl Lowering<'_, '_> {
    /// The group of a type, its members in the order a search from the first member asked
    /// for finds them; none for a type whose release does not reach itself.
    pub fn release_group(&mut self, ty: &Ty) -> Option<Rc<Vec<Ty>>> {
        if let Some(group) = self.release_groups.get(ty) {
            return group.clone();
        }
        let group = group_of(self, ty);
        match &group {
            Some(members) => {
                for member in members.iter() {
                    self.release_groups.insert(member.clone(), group.clone());
                }
            }
            None => {
                self.release_groups.insert(ty.clone(), None);
            }
        }
        group
    }
}

/// The types of the values a type's release releases through no other type's release: what a
/// holder holds, or the fields, payloads and elements of a type laid out in full.
fn parts(lo: &mut Lowering, ty: &Ty) -> Vec<Ty> {
    if !lo.needs_release(ty) {
        return Vec::new();
    }
    if let Some(holder) = Holder::of(lo, ty) {
        let held = holder.held().clone();
        return match lo.needs_release(&held) {
            true => vec![held],
            false => Vec::new(),
        };
    }
    let Ty::Item(item, _) = ty else {
        return Vec::new();
    };
    if lo.ck.db.is_std(*item) {
        let path = lo.ck.db.canonical(*item);
        if supplied::base_needs_release(lo, &path, ty).is_some() {
            return Vec::new();
        }
    }
    let layout = lo.layout(ty);
    let parts: Vec<Ty> = match &layout.shape {
        Shape::Record(fields) | Shape::Bytes { fields, .. } => {
            fields.iter().map(|field| field.ty.clone()).collect()
        }
        Shape::Enum(shape) => shape
            .variants
            .iter()
            .flat_map(|variant| variant.fields.iter().map(|field| field.ty.clone()))
            .collect(),
        Shape::Array { element, .. } => vec![element.clone()],
        _ => Vec::new(),
    };
    parts
        .into_iter()
        .filter(|part| lo.needs_release(part))
        .collect()
}

/// The types a search through parts from a type finds and that reach it, in the order found.
fn group_of(lo: &mut Lowering, ty: &Ty) -> Option<Rc<Vec<Ty>>> {
    let mut found = vec![ty.clone()];
    let mut indexes = HashMap::from([(ty.clone(), 0)]);
    let mut edges: Vec<Vec<usize>> = Vec::new();
    while edges.len() < found.len() {
        let mut targets = Vec::new();
        for part in parts(lo, &found[edges.len()].clone()) {
            let index = *indexes.entry(part.clone()).or_insert_with(|| {
                found.push(part);
                found.len() - 1
            });
            targets.push(index);
        }
        edges.push(targets);
    }
    let mut reaches = vec![false; found.len()];
    let mut changed = true;
    while changed {
        changed = false;
        for (index, targets) in edges.iter().enumerate() {
            if !reaches[index]
                && targets
                    .iter()
                    .any(|target| *target == 0 || reaches[*target])
            {
                reaches[index] = true;
                changed = true;
            }
        }
    }
    if !reaches[0] {
        return None;
    }
    let members = found
        .into_iter()
        .zip(reaches)
        .filter_map(|(member, reaches)| reaches.then_some(member))
        .collect();
    Some(Rc::new(members))
}

/// Releases the value of a group's type at an address through the group's walk.
pub fn release(lo: &mut Lowering, fx: &mut Fx, group: &Rc<Vec<Ty>>, ty: &Ty, address: Value) {
    let members = group.clone();
    let mut signature = Signature::new(CALL_CONV);
    signature.params.push(AbiParam::new(PTR));
    signature.params.push(AbiParam::new(PTR));
    let walk = lo.glue_function(Glue::Release(group[0].clone()), signature, move |lo, fx| {
        define(lo, fx, &members)
    });
    let member = group
        .iter()
        .position(|member| member == ty)
        .expect("a member of the group");
    let reference = lo.call_ref(fx, walk);
    let member = fx.ptr(member as u64);
    fx.b.ins().call(reference, &[address, member]);
}

/// The blocks of a walk, each taking a frame's address, its parent's address and its parent's
/// index: per member, entering a frame of it and searching a frame of it for a holder still
/// holding something; and leaving a frame for its parent.
struct Walk {
    group: Rc<Vec<Ty>>,
    enter: Vec<Block>,
    search: Vec<Block>,
}

/// What a walk does at a holder.
#[derive(Clone, Copy)]
enum Step {
    /// Entering a frame: frees an empty vector's storage and a hash map's index.
    Enter,
    /// Searching a frame with its parent and its parent's index: enters the next value held.
    Search { parent: Value, index: Value },
    /// Leaving a frame for its parent: takes it out of the first holder still holding one.
    Leave { child: Value },
}

/// The walk releasing a value of a group's type at the address of its first parameter, the
/// type by its index in the group in the second.
fn define(lo: &mut Lowering, fx: &mut Fx, group: &Rc<Vec<Ty>>) {
    let params = fx.entry();
    let (root, index) = (params[0], params[1]);
    let mut enter = Vec::new();
    let mut search = Vec::new();
    for _ in group.iter() {
        enter.push(frame_block(fx));
        search.push(frame_block(fx));
    }
    let leave = frame_block(fx);
    let walk = Walk {
        group: group.clone(),
        enter,
        search,
    };
    dispatch(fx, index, group.len(), |fx, member| {
        fx.b.ins().jump(
            walk.enter[member],
            &[root.into(), root.into(), index.into()],
        );
    });
    for (member, ty) in group.iter().enumerate() {
        fx.b.switch_to_block(walk.enter[member]);
        let frame = fx.b.block_params(walk.enter[member]).to_vec();
        walk_value(lo, fx, &walk, Step::Enter, member, frame[0], ty, frame[0]);
        let args: Vec<_> = frame.iter().map(|value| (*value).into()).collect();
        fx.b.ins().jump(walk.search[member], &args);
        fx.b.switch_to_block(walk.search[member]);
        let frame = fx.b.block_params(walk.search[member]).to_vec();
        let step = Step::Search {
            parent: frame[1],
            index: frame[2],
        };
        walk_value(lo, fx, &walk, step, member, frame[0], ty, frame[0]);
        let args: Vec<_> = frame.iter().map(|value| (*value).into()).collect();
        fx.b.ins().jump(leave, &args);
    }
    fx.b.switch_to_block(leave);
    let frame = fx.b.block_params(leave).to_vec();
    let finished = fx.b.ins().icmp(IntCC::Equal, frame[0], root);
    fx.when(finished, |fx| {
        fx.b.ins().return_(&[]);
    });
    dispatch(fx, frame[2], group.len(), |fx, member| {
        let step = Step::Leave { child: frame[0] };
        walk_value(
            lo,
            fx,
            &walk,
            step,
            member,
            frame[1],
            &group[member],
            frame[1],
        );
        fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
    });
}

/// A block taking a frame's address, its parent's address and its parent's index.
fn frame_block(fx: &mut Fx) -> Block {
    let block = fx.block();
    for _ in 0..3 {
        fx.b.append_block_param(block, PTR);
    }
    block
}

/// Runs `each` for the index below `count` a value equals, the last when it equals no other.
fn dispatch(fx: &mut Fx, value: Value, count: usize, mut each: impl FnMut(&mut Fx, usize)) {
    for index in 0..count - 1 {
        let is = fx.b.ins().icmp_imm_s(IntCC::Equal, value, index as i64);
        fx.when(is, |fx| each(fx, index));
    }
    each(fx, count - 1);
}

/// What a step does at the value of a group's type at `address`, in the frame of the
/// group's `member` at `frame`.
#[allow(clippy::too_many_arguments)]
fn walk_value(
    lo: &mut Lowering,
    fx: &mut Fx,
    walk: &Walk,
    step: Step,
    member: usize,
    frame: Value,
    ty: &Ty,
    address: Value,
) {
    if let Some(holder) = Holder::of(lo, ty) {
        held(lo, fx, walk, step, member, frame, &holder, address);
        return;
    }
    let layout = lo.layout(ty);
    match &layout.shape {
        Shape::Record(fields) | Shape::Bytes { fields, .. } => {
            for field in fields.clone() {
                let at = fx.offset(address, field.offset);
                part(
                    lo,
                    fx,
                    walk,
                    step,
                    member,
                    frame,
                    (ty, &field.name),
                    &field.ty,
                    at,
                );
            }
        }
        Shape::Enum(shape) => {
            for variant in &shape.variants {
                let fields: Vec<_> = variant
                    .fields
                    .iter()
                    .filter(|field| match step {
                        Step::Enter => lo.needs_release(&field.ty),
                        _ => walk.group.contains(&field.ty),
                    })
                    .cloned()
                    .collect();
                if fields.is_empty() {
                    continue;
                }
                let is = lo.variant_is(fx, address, ty, &variant.name);
                fx.when(is, |fx| {
                    for field in &fields {
                        let at = fx.offset(address, field.offset);
                        let named = (ty, variant.name.as_str());
                        part(lo, fx, walk, step, member, frame, named, &field.ty, at);
                    }
                });
            }
        }
        Shape::Array { element, count } => {
            let element = element.clone();
            let walked = match step {
                Step::Enter => lo.needs_release(&element),
                _ => walk.group.contains(&element),
            };
            if walked {
                let size = lo.layout(&element).size;
                let start = fx.ptr(0);
                let end = fx.ptr(*count);
                fx.each(start, end, |fx, index| {
                    let offset = fx.b.ins().imul_imm_s(index, size as i64);
                    let at = fx.b.ins().iadd(address, offset);
                    part(lo, fx, walk, step, member, frame, (ty, ""), &element, at);
                });
            }
        }
        _ => {}
    }
}

/// What a step does at a part of a value, `named` by its owner and the field or variant
/// declaring it: walks a part of a group type, and releases another on entering.
#[allow(clippy::too_many_arguments)]
fn part(
    lo: &mut Lowering,
    fx: &mut Fx,
    walk: &Walk,
    step: Step,
    member: usize,
    frame: Value,
    named: (&Ty, &str),
    ty: &Ty,
    address: Value,
) {
    if walk.group.contains(ty) {
        walk_value(lo, fx, walk, step, member, frame, ty, address);
    } else if let Step::Enter = step {
        lo.release_part(fx, named.0, named.1, ty, address);
    }
}

/// What a step does at a holder of the frame of the group's `member` at `frame`.
#[allow(clippy::too_many_arguments)]
fn held(
    lo: &mut Lowering,
    fx: &mut Fx,
    walk: &Walk,
    step: Step,
    member: usize,
    frame: Value,
    holder: &Holder,
    address: Value,
) {
    match (step, holder) {
        (Step::Enter, Holder::Vector { indexed, .. }) => {
            if *indexed {
                maps::free_index(lo, fx, address);
            }
            let len = fx.load(PTR, address, vector::LEN);
            let empty = fx.b.ins().icmp_imm_s(IntCC::Equal, len, 0);
            fx.when(empty, |fx| vector::free_storage(lo, fx, address));
        }
        (Step::Enter, _) => {}
        (Step::Search { parent, index }, Holder::Vector { element, .. }) => {
            let len = fx.load(PTR, address, vector::LEN);
            let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
            fx.when(more, |fx| {
                let data = fx.load(PTR, address, vector::DATA);
                let last = fx.b.ins().iadd_imm_s(len, -1);
                let child = vector::Element::of(lo, element).at(fx, data, last);
                let link = link(fx, parent, index);
                fx.store(link, address, vector::ALLOC);
                enter(fx, walk, element, child, member, frame);
            });
        }
        (Step::Search { parent, index }, Holder::Boxed(contents)) => {
            let word = fx.load(PTR, address, 0);
            let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, word, DONE);
            fx.when(more, |fx| {
                let link = link(fx, parent, index);
                fx.store(link, address, 0);
                enter(fx, walk, contents, word, member, frame);
            });
        }
        (Step::Search { parent, index }, Holder::Shared(contents)) => {
            let word = fx.load(PTR, address, 0);
            let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, word, DONE);
            fx.when(more, |fx| {
                let minus = fx.ptr(u64::MAX);
                let before = fx.b.ins().atomic_rmw(
                    PTR,
                    MemFlagsData::trusted(),
                    AtomicRmwOp::Add,
                    word,
                    minus,
                );
                let shared = fx.b.ins().icmp_imm_s(IntCC::NotEqual, before, 1);
                fx.when(shared, |fx| {
                    let done = fx.ptr(DONE as u64);
                    fx.store(done, address, 0);
                    fx.b.ins().jump(
                        walk.search[member],
                        &[frame.into(), parent.into(), index.into()],
                    );
                });
                let at = stores::contents_at(lo, contents);
                let child = fx.offset(word, at);
                let link = link(fx, parent, index);
                fx.store(link, address, 0);
                enter(fx, walk, contents, child, member, frame);
            });
        }
        (Step::Leave { .. }, Holder::Vector { .. }) => {
            let len = fx.load(PTR, address, vector::LEN);
            let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, len, 0);
            fx.when(more, |fx| {
                let link = fx.load(PTR, address, vector::ALLOC);
                let left = fx.b.ins().iadd_imm_s(len, -1);
                fx.store(left, address, vector::LEN);
                let empty = fx.b.ins().icmp_imm_s(IntCC::Equal, left, 0);
                fx.when(empty, |fx| {
                    let data = fx.load(PTR, address, vector::DATA);
                    lo.call_routine(fx, Routine::Free, &[data]);
                });
                resume(fx, walk, member, frame, link);
            });
        }
        (Step::Leave { child }, Holder::Boxed(_) | Holder::Shared(_)) => {
            let word = fx.load(PTR, address, 0);
            let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, word, DONE);
            fx.when(more, |fx| {
                let storage = match holder {
                    Holder::Shared(contents) => {
                        let at = stores::contents_at(lo, contents);
                        fx.b.ins().iadd_imm_s(child, -(at as i64))
                    }
                    _ => child,
                };
                lo.call_routine(fx, Routine::Free, &[storage]);
                let done = fx.ptr(DONE as u64);
                fx.store(done, address, 0);
                resume(fx, walk, member, frame, word);
            });
        }
    }
}

/// The link naming a frame by its address and its type's index in the group.
fn link(fx: &mut Fx, frame: Value, index: Value) -> Value {
    let address = fx.b.ins().band_imm_s(frame, (1 << ADDRESS_BITS) - 1);
    let index = fx.b.ins().ishl_imm_s(index, ADDRESS_BITS);
    fx.b.ins().bor(address, index)
}

/// Enters a value of a group type at `child`, held by the frame of the group's `member` at
/// `frame`.
fn enter(fx: &mut Fx, walk: &Walk, held: &Ty, child: Value, member: usize, frame: Value) {
    let index = walk
        .group
        .iter()
        .position(|ty| ty == held)
        .expect("a member of the group");
    let parent = fx.ptr(member as u64);
    fx.b.ins().jump(
        walk.enter[index],
        &[child.into(), frame.into(), parent.into()],
    );
}

/// Goes on searching the frame of the group's `member` at `frame`, whose parent a link names.
fn resume(fx: &mut Fx, walk: &Walk, member: usize, frame: Value, link: Value) {
    let shifted = fx.b.ins().ishl_imm_s(link, 64 - ADDRESS_BITS);
    let parent = fx.b.ins().sshr_imm_s(shifted, 64 - ADDRESS_BITS);
    let index = fx.b.ins().ushr_imm_s(link, ADDRESS_BITS);
    fx.b.ins().jump(
        walk.search[member],
        &[frame.into(), parent.into(), index.into()],
    );
}
