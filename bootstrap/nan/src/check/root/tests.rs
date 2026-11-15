use std::collections::HashMap;
use std::path::PathBuf;

use super::coded_key;
use crate::hash::Hash;
use crate::workspace::{Member, Workspace};

const DIAL: &str = "//! Dials.\n\n/// The notch.\npub fn notch() -> u32 = 7\n";
const APP: &str = "//! Apps.\n\npub fn notch() -> u32 = dial::notch()\n";

/// The key of the code root of `dial` in the closure of `app`, which reaches it.
fn dial_key(workspace: &Workspace) -> Hash {
    let index = |path: &str| workspace.file_package(path).expect("a package");
    let app = &workspace.packages[index("app/app.nan")];
    let closure = workspace.closure_of(app).expect("the closure of app");
    coded_key(workspace, closure, Member::Source(index("dial/dial.nan")))
}

#[test]
fn a_code_root_is_kept_under_the_states_its_package_reaches() {
    let workspace = Workspace::of_files(
        PathBuf::from("/w"),
        &[
            (
                "dial/package.nan",
                "//! Dials.\n\npackage dial {\n    language 1\n    targets any\n} dial\n".as_bytes(),
            ),
            ("dial/dial.nan", DIAL.as_bytes()),
            (
                "app/package.nan",
                "//! Apps.\n\npackage app {\n    language 1\n    targets any\n    dep dial \"../dial\"\n} app\n".as_bytes(),
            ),
            ("app/app.nan", APP.as_bytes()),
        ],
    );
    let edited = |path: &str, text: &str| {
        workspace.with_sources(&HashMap::from([(
            path.to_owned(),
            text.as_bytes().to_vec(),
        )]))
    };
    let kept = dial_key(&workspace);
    assert_eq!(
        dial_key(&edited(
            "app/app.nan",
            &APP.replace("notch()\n", "notch() + 1\n")
        )),
        kept,
        "editing a package the dependency does not reach keeps its key"
    );
    assert_ne!(
        dial_key(&edited("dial/dial.nan", &DIAL.replace('7', "8"))),
        kept
    );
}
