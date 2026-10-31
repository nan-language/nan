use crate::check::ty::Ty;
use crate::check::unchecked;
use crate::workspace::tests::Scratch;
use crate::workspace::{Target, Workspace};

const MANIFEST: &str =
    "//! Specification example.\n\npackage example {\n    language 1\n    targets any\n} example\n";

/// An exported record holding an option of a private record, a private record and a generic one.
const TYPES: &str = "//! Specification example.\n\npub closed struct Outer derives Encode {\n    pub inner: Option[Inner]\n    pub level: u8\n} Outer\n\nclosed struct Inner derives Encode {\n    pub count: Option[u16]\n} Inner\n\npub closed struct Pair[T] derives Encode {\n    pub left: T\n} Pair\n";

#[test]
fn code_roots_record_exported_trees_and_the_foreign_instantiations_outside_others() {
    let scratch = Scratch::new(&[
        ("example/package.nan", MANIFEST),
        ("example/example.nan", TYPES),
    ]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let checker = unchecked(
        &workspace,
        &workspace.closures[0],
        crate::stdlib::packages(),
        Target::Any,
    );
    let package = checker.package_named("example").expect("the package");
    let recorded = checker.recorded_schemas(package);
    assert_eq!(recorded.keys().collect::<Vec<_>>(), ["example::Outer"]);
    let foreign = checker.foreign_schemas(&recorded["example::Outer"], package);
    let [Ty::Item(option, args)] = &foreign[..] else {
        panic!("one foreign instantiation, not {foreign:?}");
    };
    assert_eq!(checker.db.canonical(*option), "core::Option");
    let entry = checker
        .requires_set(package, &|_| 0)
        .into_iter()
        .find(|entry| entry.path == "core::Option")
        .expect("an entry for Option");
    let hash = checker.schema(&Ty::Item(*option, args.clone())).hash();
    assert_eq!(entry.schemas.into_iter().collect::<Vec<_>>(), [hash]);
}
