use super::build;
use crate::diagnostic::Code;
use crate::workspace::Workspace;
use crate::workspace::tests::Scratch;

#[test]
fn a_symbol_no_library_defines_fails_the_link() {
    let scratch = Scratch::new(&[
        (
            "probe/package.nan",
            "//! A program.\n\npackage probe {\n    language 1\n    targets x86_64-linux\n    entry main\n    allow trusted in probe\n} probe\n",
        ),
        (
            "probe/probe.nan",
            "//! Calls a symbol no library defines.\n\nforeign fn nan_probe_undefined() -> i32 convention \"sysv\"\n\nfn main(take world: host::World) fails task::Cancelled uses wait {\n    discard nan_probe_undefined()\n    try await world.clock.sleep(1)\n} main\n",
        ),
    ]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let built = build(&workspace, 0, None, &[]).expect("a final build");
    let reported: Vec<(Code, Option<u32>)> = (built.diagnostics.iter())
        .map(|diagnostic| (diagnostic.code, diagnostic.line))
        .collect();
    assert_eq!(reported, [(Code::TargetLinkFailed, Some(6))]);
    assert!(built.image.is_none());
}

#[test]
fn a_bundle_build_reports_the_errors_of_its_package_before_encoding_it() {
    let scratch = Scratch::new(&[
        (
            "kernel/package.nan",
            "//! A kernel exporting its load ABI.\n\npackage kernel {\n    language 1\n    targets x86_64-bare\n    entry main\n} kernel\n",
        ),
        (
            "kernel/kernel.nan",
            "//! Entry of the kernel.\n\nfn main(take k: boot::Kernel) -> Never limits stack 16_384 {\n    discard k.loader()\n    loop {\n    }\n} main\n",
        ),
        (
            "tally/package.nan",
            "//! A tally.\n\npackage tally {\n    language 1\n    targets x86_64-nanos\n    entry Tally\n} tally\n",
        ),
        ("tally/tally.nan", TALLY),
    ]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let index = |name: &str| {
        (workspace.packages.iter())
            .position(|package| package.name() == Some(name))
            .expect("the package")
    };
    let kernel = build(&workspace, index("kernel"), None, &[]).expect("the kernel's build");
    let built = build(&workspace, index("tally"), None, &kernel.companions).expect("a build");
    let reported: Vec<(Code, Option<u32>)> = (built.diagnostics.iter())
        .map(|diagnostic| (diagnostic.code, diagnostic.line))
        .collect();
    assert_eq!(
        reported,
        [
            (Code::SyntaxUnexpectedToken, Some(4)),
            (Code::InterfaceMissingMethod, Some(19))
        ]
    );
    assert!(built.image.is_none());
}

/// A component serving a protocol whose operation's limits clause does not parse, through a
/// handler whose head names that protocol.
const TALLY: &str = "//! A tally.\n\npub protocol Counter {\n    pub fn add(amount: u32) -> u64 limits none\n} Counter\n\npub closed struct Env {\n    pub clock: sys::Clock\n} Env\n\nimpl Env: nanos::Environment\n\npub fn Env.build(take world: nanos::World) -> Env = Env(clock: world.clock)\n\npub closed struct Tally derives Encode {\n    pub total: u64\n} Tally\n\nimpl Tally: sys::Component[Env, Counter]\n\npub fn Tally.start(mut env: Env) -> Tally = Tally(total: 0)\n\npub fn Tally.add(mut self, take amount: u32, mut env: Env, mut cx: sys::Cx[Counter]) -> u64 {\n    self.total = self.total.wrapping_add(u64.widen(amount))\n    return self.total\n} Tally.add\n";

/// The tree hash of the program `tool` built for a target, whose library `lib` has a variant of
/// its module `os` for each system.
fn tool_tree(target: &str) -> crate::hash::Hash {
    let manifest = format!(
        "//! A program.\n\npackage tool {{\n    language 1\n    targets {target}\n    entry main\n    dep lib \"../lib\"\n}} tool\n"
    );
    let scratch = Scratch::new(&[
        ("tool/package.nan", manifest.as_str()),
        (
            "tool/tool.nan",
            "//! Exits with its system's code.\n\nfn main(take world: host::World) -> u8 fails task::Cancelled uses wait {\n    try await world.clock.sleep(1)\n    return lib::system()\n} main\n",
        ),
        (
            "lib/package.nan",
            "//! A library with a variant per system.\n\npackage lib {\n    language 1\n    targets x86_64-linux, x86_64-windows\n} lib\n",
        ),
        (
            "lib/lib.nan",
            "//! The library.\n\n/// The code of the system.\npub fn system() -> u8 = os::code()\n",
        ),
        (
            "lib/os@linux/os.nan",
            "//! The system.\n\n/// Its code.\npub fn code() -> u8 = 1\n",
        ),
        (
            "lib/os@windows/os.nan",
            "//! The system.\n\n/// Its code.\npub fn code() -> u8 = 2\n",
        ),
    ]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let source = (workspace.packages.iter())
        .find(|package| package.name() == Some("tool"))
        .expect("the program");
    let closure = workspace.closure_of(source).expect("the program's closure");
    let std = crate::stdlib::packages();
    let checker = crate::check::checked(&workspace, closure, std, source.program_target());
    let root = super::package_id(&checker, source);
    super::closure_files(&workspace, &checker, root).tree
}

#[test]
fn the_builds_of_one_source_tree_for_two_targets_share_its_tree_hash() {
    assert_eq!(tool_tree("x86_64-linux"), tool_tree("x86_64-windows"));
}
