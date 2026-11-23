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

/// The artefacts of a closure of one program package whose manifest is `manifest`.
fn closure_of(manifest: &str) -> std::collections::BTreeMap<String, Vec<u8>> {
    use crate::artefact::{Parts, artefact, attestation, code_root, seal_list, source_tree_hash};
    let parts = Parts {
        language: 1,
        manifest: manifest.to_owned(),
        interface: Vec::new(),
        nir: Vec::new(),
        requires: Vec::new(),
        source_tombstones: Vec::new(),
        history_tombstones: Vec::new(),
        ledger: Vec::new(),
        lines: Vec::new(),
        schemas: Vec::new(),
        protocols: Vec::new(),
        earlier: Vec::new(),
    };
    let attested = attestation(
        crate::hash::Hash::of(b"toolchain"),
        source_tree_hash(&[]),
        &seal_list(&[]),
    );
    let artefact = artefact(&code_root(&parts), &attested);
    std::collections::BTreeMap::from([("tool".to_owned(), artefact)])
}

#[test]
fn the_builds_of_one_source_tree_for_two_targets_share_its_tree_hash() {
    let manifest = |targets: &str, entry: &str| {
        format!(
            "package tool {{\n    language 1\n    targets {targets}\n    entry {entry}\n}} tool\n"
        )
    };
    let linux = super::tree(&closure_of(&manifest("x86_64-linux", "main")));
    let nanos = super::tree(&closure_of(&manifest("x86_64-nanos", "main")));
    let other = super::tree(&closure_of(&manifest("x86_64-nanos", "start")));
    assert_eq!(linux, nanos);
    assert_ne!(nanos, other);
}
