use cranelift_module::FuncId;

use super::*;
use crate::check::checked;
use crate::nir::Span;
use crate::workspace::tests::Scratch;
use crate::workspace::{Target, Workspace};

#[test]
fn an_exceeded_bound_names_each_call_of_the_deepest_path_in_source() {
    let scratch = Scratch::new(&[
        (
            "app/package.nan",
            "//! Calls.\n\npackage app {\n    language 1\n    targets x86_64-host\n} app\n",
        ),
        (
            "app/app.nan",
            "//! Calls.\n\npub fn outer() -> u64 {\n    return inner()\n} outer\n\npub fn inner() -> u64 {\n    return 1\n} inner\n",
        ),
    ]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let checker = checked(
        &workspace,
        &workspace.closures[0],
        crate::stdlib::packages(),
        Target::X64Host,
    );
    let item = |canonical: &str| {
        (0..checker.db.items.len())
            .map(|index| ItemId(index as u32))
            .find(|item| checker.db.canonical(*item) == canonical)
            .expect("the function")
    };
    let outer = item("app::outer");
    let at = At {
        item: outer,
        span: Span::at(checker.db.ast(outer).token),
    };
    let (outer_fn, inner_fn) = (FuncId::from_u32(0), FuncId::from_u32(1));
    let frames = [
        Frame {
            function: outer_fn,
            name: "app::outer".to_owned(),
            size: 64,
            params: Vec::new(),
            sites: vec![Site {
                kind: Kind::Direct {
                    callee: inner_fn,
                    bindings: Vec::new(),
                },
                at: Some(at),
            }],
        },
        Frame {
            function: inner_fn,
            name: "app::inner".to_owned(),
            size: 64,
            params: Vec::new(),
            sites: vec![Site {
                kind: Kind::Stub,
                at: None,
            }],
        },
    ];
    let demand = Demand {
        judged: Judged::Function(outer_fn),
        stack: 100,
        report: Report::Own { head: at },
    };
    let found = judge(&checker, &frames, &[demand]);
    let because: Vec<(Role, &str)> = found[0]
        .because
        .iter()
        .map(|reason| (reason.role, reason.subject.as_str()))
        .collect();
    assert_eq!(found[0].code, Code::LimitStackExceeded);
    assert_eq!(because, [(Role::Called, "app::inner")]);
}

/// A recursion's thunk needs its callee's frame and the wrapper's frame with the stub call bound
/// at the grown call, and a function value's thunk the most any other function needs.
#[test]
fn a_grown_call_needs_the_stack_up_to_the_next_grown_call() {
    let [recursive, wrapper, thunk, value_thunk, big] = [0, 1, 2, 3, 4].map(FuncId::from_u32);
    let frame = |function, size, sites: Vec<Kind>| Frame {
        function,
        name: String::new(),
        size,
        params: Vec::new(),
        sites: sites
            .into_iter()
            .map(|kind| Site { kind, at: None })
            .collect(),
    };
    let frames = [
        frame(
            recursive,
            48,
            vec![Kind::Grown {
                wrapper,
                thunk,
                inner: Box::new(Kind::Direct {
                    callee: recursive,
                    bindings: Vec::new(),
                }),
            }],
        ),
        frame(wrapper, 64, vec![Kind::Stub]),
        frame(
            thunk,
            16,
            vec![Kind::Direct {
                callee: recursive,
                bindings: Vec::new(),
            }],
        ),
        frame(value_thunk, 32, vec![Kind::Bounded(None)]),
        frame(big, 4_096, vec![Kind::Bounded(Some(100))]),
    ];
    let grown = [
        Grown {
            thunk,
            callee: Some(recursive),
        },
        Grown {
            thunk: value_thunk,
            callee: None,
        },
    ];
    let excluded = HashSet::from([wrapper, thunk, value_thunk]);
    let needs = needs(&frames, &grown, &excluded);
    assert_eq!(needs, [16 + 48 + 64 + STUB_CALL_BOUND, 32 + 4_096 + 100]);
}
