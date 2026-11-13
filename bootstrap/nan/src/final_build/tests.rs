use super::build;
use crate::diagnostic::Code;
use crate::workspace::Workspace;
use crate::workspace::tests::Scratch;

#[test]
fn a_symbol_no_library_defines_fails_the_link() {
    let scratch = Scratch::new(&[
        (
            "probe/package.nan",
            "//! A program.\n\npackage probe {\n    language 1\n    targets x86_64-host\n    entry main\n    allow trusted in probe\n} probe\n",
        ),
        (
            "probe/probe.nan",
            "//! Calls a symbol no library defines.\n\nforeign fn nan_probe_undefined() -> i32 convention \"sysv\"\n\nfn main(take world: host::World) fails task::Cancelled uses wait {\n    discard nan_probe_undefined()\n    try await world.clock.sleep(1)\n} main\n",
        ),
    ]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let built = build(&workspace, 0, None).expect("a final build");
    let reported: Vec<(Code, Option<u32>)> = (built.diagnostics.iter())
        .map(|diagnostic| (diagnostic.code, diagnostic.line))
        .collect();
    assert_eq!(reported, [(Code::TargetLinkFailed, Some(6))]);
    assert!(built.image.is_none());
}
