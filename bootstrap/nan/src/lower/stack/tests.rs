use cranelift_module::FuncId;

use super::*;
use crate::check::checked;
use crate::nir::Span;
use crate::workspace::tests::Scratch;
use crate::workspace::{Target, Workspace};

/// Runs `test` with the checker of a package whose function `app::outer` calls `app::inner`,
/// and a place in `outer`.
fn with_outer(test: impl FnOnce(&Checker, At)) {
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
    test(
        &checker,
        At {
            item: outer,
            span: Span::at(checker.db.ast(outer).token),
        },
    );
}

/// The reasons of a diagnostic, by role and subject.
fn reasons(found: &Diagnostic) -> Vec<(Role, &str)> {
    (found.because.iter())
        .map(|reason| (reason.role, reason.subject.as_str()))
        .collect()
}

#[test]
fn an_exceeded_bound_names_each_call_of_the_deepest_path_in_source() {
    with_outer(|checker, at| {
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
        let found = judge(checker, &frames, &[demand]);
        assert_eq!(found[0].code, Code::LimitStackExceeded);
        assert_eq!(reasons(&found[0]), [(Role::Called, "app::inner")]);
    });
}

#[test]
fn an_unbounded_closure_argument_names_the_calls_down_to_its_cause() {
    with_outer(|checker, at| {
        let (outer_fn, apply_fn, closure_fn) = (
            FuncId::from_u32(0),
            FuncId::from_u32(1),
            FuncId::from_u32(2),
        );
        let frame = |function, name: &str, params, kind| Frame {
            function,
            name: name.to_owned(),
            size: 64,
            params,
            sites: vec![Site { kind, at: None }],
        };
        let frames = [
            frame(
                outer_fn,
                "app::outer",
                Vec::new(),
                Kind::Direct {
                    callee: apply_fn,
                    bindings: vec![(0, Binding::Function(closure_fn))],
                },
            ),
            frame(apply_fn, "app::apply", vec![(0, None)], Kind::Param(0)),
            frame(closure_fn, "", Vec::new(), Kind::Bounded(None)),
        ];
        let demand = Demand {
            judged: Judged::Function(outer_fn),
            stack: 4096,
            report: Report::Own { head: at },
        };
        let found = judge(checker, &frames, &[demand]);
        assert_eq!(found[0].code, Code::LimitStackUnbounded);
        assert_eq!(
            reasons(&found[0]),
            [
                (Role::Called, "app::apply"),
                (Role::Called, "function parameter"),
                (Role::Raised, "function value"),
            ]
        );
    });
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
