use super::*;
use crate::check::ty::Scalar;
use crate::check::unchecked;
use crate::nir::{BlockId, Role};
use crate::workspace::Target;

/// Calls a member of a `mem::Pool[u8, Unit]`.
fn pool_member(interp: &mut Interpreter, member: &str, args: &[Val]) -> Val {
    let type_args = [Arg::Ty(Ty::Scalar(Scalar::U8)), Arg::Ty(Ty::Unit)];
    let site = Site {
        role: Role::Body,
        index: 0,
        block: BlockId(0),
        position: 0,
    };
    let at = (ItemId(0), Span::at(0));
    match super::member(interp, "mem::Pool", member, &type_args, args, at, site) {
        Some(Ok(Flow::Return(value))) => value,
        _ => panic!("mem::Pool.{member} returning"),
    }
}

#[test]
fn a_pool_slot_freed_at_generation_2_32_minus_1_is_retired() {
    let std = crate::stdlib::workspace();
    let checker = unchecked(std, &std.closures[0], &[], Target::Any);
    let mut interp = Interpreter::new(&checker);
    let cell = interp.budgets.root(WORLD_LIMIT);
    let last = slot(Val::Int(RETIRED - 1), Val::Absent);
    let pool = pool(&mut interp, vec![last], cell);
    let place = Loc {
        extent: interp.memory.alloc(pool, true, 0),
        path: Vec::new(),
    };
    let freed = Val::Array(Rc::new(vec![Val::Int(0)]));
    interp.memory.set(&place.with(FREED as u32), freed);
    let pool = Val::Ref(place);
    let first = pool_member(&mut interp, "insert", &[pool.clone(), Val::Int(1)]);
    assert_eq!(*first.fields(), [Val::Int(0), Val::Int(RETIRED - 1)]);
    pool_member(&mut interp, "remove", &[pool.clone(), first.clone()]);
    let second = pool_member(&mut interp, "insert", &[pool.clone(), Val::Int(2)]);
    assert_eq!(*second.fields(), [Val::Int(1), Val::Int(0)]);
    let stale = pool_member(&mut interp, "contains", &[pool.clone(), first]);
    assert_eq!(stale, Val::Bool(false));
    let slots = entries(&interp, &interp.deref(&pool));
    assert_eq!(*slots[0].fields(), [Val::Int(RETIRED), Val::Absent]);
}
