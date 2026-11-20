use std::collections::HashMap;
use std::path::PathBuf;

use super::coded_key;
use crate::artefact::Earlier;
use crate::check::unchecked;
use crate::hash::Hash;
use crate::workspace::tests::Scratch;
use crate::workspace::{Member, Target, Workspace};

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

#[test]
fn a_code_root_records_the_earlier_schemas_decode_from_reads() {
    let scratch = Scratch::new(&[
        (
            "settings/package.nan",
            "//! Settings.\n\npackage settings {\n    language 1\n    targets any\n} settings\n",
        ),
        ("settings/.nan/history.nan", HISTORY),
        ("settings/settings.nan", SETTINGS),
    ]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let checker = unchecked(
        &workspace,
        &workspace.closures[0],
        crate::stdlib::packages(),
        Target::Any,
    );
    let package = checker.package_named("settings").expect("the package");
    let roots = checker.earlier_roots(package);
    let hash = |form: &str| {
        Hash(
            crate::hash::parse_form(form)
                .expect("a hash")
                .try_into()
                .expect("33 bytes"),
        )
    };
    let limits = hash("afwsm4kkvgahmfrknqs5amzv3uj4lryliobcfzemo6czknryqyfgykn");
    let settings = hash("agib35pkkvbgllndccsmcj2cjah4xoif2okmhv7ggxbrttxqz7p5kvz");
    let recorded: Vec<(&str, &[(Hash, Earlier)])> = roots
        .iter()
        .map(|root| (root.path.as_str(), root.schemas.as_slice()))
        .collect();
    assert_eq!(
        recorded,
        [
            ("settings::Limits", &[(limits, Earlier::Accepted)][..]),
            ("settings::Settings", &[(settings, Earlier::Reversible)][..])
        ]
    );
}

/// The history of the first releases of `Limits` and `Settings`, each holding one field.
const HISTORY: &str = "nan history 1\nname \"settings\"\nmodule settings\nschema Limits \"afwsm4kkvgahmfrknqs5amzv3uj4lryliobcfzemo6czknryqyfgykn\" \"aecw433emvzvsaqgkjswg33smqatwaqgmzuwk3deomvqcbahmfwgsyltmvzqcaajmrswmylvnr2gkzabaabwwzlzaycwg33vnz2ak5tbnr2wkaibarxxazloaeaaqvloonuwo3tfmqaqsaifo5uwi5diaeeanu\"\nschema Settings \"agib35pkkvbgllndccsmcj2cjah4xoif2okmhv7ggxbrttxqz7p5kvz\" \"aecw433emvzvwaqgkjswg33smqat2aqgmzuwk3deomwqcbahmfwgsyltmvzqcaajmrswmylvnr2gkzabaabwwzlzbadxezluojuwk4yfozqwy5lfaeaqi33qmvxacaaikvxhg2lhnzswiaijaecxo2leoruaccasa\"\ncheck \"ahkk6gbuuwn2jj3ihkdadsxffwnjkttuqn64zx2bel7iem7p7qn7wc3\"\n";

/// Limits that gained a field their first release lacked, and settings with a migration from the
/// shape of their first release and its inverse.
const SETTINGS: &str = "//! Settings.\n\npub open struct Limits derives Encode {\n    pub count: u8\n    pub cap: Option[u16]\n} Limits\n\npub closed struct Settings derives Encode {\n    pub retries: u8\n    pub backoff: u16\n} Settings\n\nstruct FirstSettings derives Encode {\n    retries: u8\n} FirstSettings\n\nmigrate fn Settings.from_first(take old: FirstSettings) -> Settings limits notrap, terminates {\n    return Settings(retries: old.retries, backoff: 100)\n} Settings.from_first\n\nmigrate fn Settings.to_first(self) -> FirstSettings limits notrap, terminates {\n    return FirstSettings(retries: self.retries)\n} Settings.to_first\n";
