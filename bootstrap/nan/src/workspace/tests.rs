use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// A directory under the system's temporary directory, removed when dropped.
pub(crate) struct Scratch(pub PathBuf);

impl Scratch {
    /// A directory holding `files`; the process's runs use a home directory of their own.
    pub(crate) fn new(files: &[(&str, &str)]) -> Scratch {
        static COUNT: AtomicUsize = AtomicUsize::new(0);
        let home = std::env::temp_dir().join("nan-test-home");
        fs::create_dir_all(&home).expect("create the home directory");
        crate::cache::use_home(home, None);
        let name = format!(
            "nan-test-{}-{}",
            std::process::id(),
            COUNT.fetch_add(1, Ordering::Relaxed)
        );
        let dir = std::env::temp_dir().join(name);
        for (path, text) in files {
            let path = dir.join(path);
            fs::create_dir_all(path.parent().expect("a file lies in a directory"))
                .expect("create a directory");
            fs::write(path, text).expect("write a file");
        }
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const MANIFEST: &str = "//! A package.\n\npackage app {\n    language 1\n    targets any\n} app\n";

fn load(files: &[(&str, &str)]) -> (Scratch, Workspace) {
    let scratch = Scratch::new(files);
    let workspace =
        Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("load the workspace");
    (scratch, workspace)
}

/// The diagnostics of reading a workspace but those of its locks.
fn codes(workspace: &Workspace) -> Vec<(String, u32, &'static str)> {
    workspace
        .diagnostics()
        .filter(|diagnostic| !diagnostic.code.name().starts_with("dep."))
        .map(|diagnostic| {
            (
                diagnostic.path.unwrap_or_default(),
                diagnostic.line.unwrap_or_default(),
                diagnostic.code.name(),
            )
        })
        .collect()
}

fn manifest(directives: &str) -> String {
    format!("//! A package.\n\npackage app {{\n{directives}}} app\n")
}

#[test]
fn the_root_is_above_the_outermost_package() {
    let scratch = Scratch::new(&[
        ("outer/package.nan", MANIFEST),
        ("outer/inner/package.nan", MANIFEST),
        ("loose/x.txt", ""),
    ]);
    assert_eq!(root(&scratch.0.join("outer/inner")), scratch.0);
    assert_eq!(root(&scratch.0.join("loose")), scratch.0.join("loose"));
    assert!(holds_package(&scratch.0).expect("walk the scratch directory"));
    assert!(!holds_package(&scratch.0.join("loose")).expect("walk the loose directory"));
}

#[test]
fn files_belong_to_the_nearest_package_and_dot_names_are_ignored() {
    let (_scratch, workspace) = load(&[
        ("app/package.nan", MANIFEST),
        ("app/app.nan", "//! Root.\n"),
        ("app/tool/package.nan", &MANIFEST.replace("app", "tool")),
        ("app/tool/tool.nan", "//! Tool.\n"),
        ("app/.hidden/ignored.nan", "not nan"),
        ("stray.nan", "//! Stray.\n"),
    ]);
    let files: Vec<&str> = workspace
        .files()
        .map(|(_, file)| file.path.as_str())
        .collect();
    assert_eq!(
        files,
        [
            "app/package.nan",
            "app/app.nan",
            "app/tool/package.nan",
            "app/tool/tool.nan"
        ]
    );
    assert_eq!(
        codes(&workspace),
        [("stray.nan".to_owned(), 1, "manifest.missing")]
    );
}

#[test]
fn manifest_directives_are_checked() {
    let cases: &[(&str, &str, u32)] = &[
        ("    language 1\n", "manifest.missing-directive", 3),
        (
            "    language 1\n    language 1\n    targets any\n",
            "manifest.duplicate",
            5,
        ),
        (
            "    language 1\n    targets any, x86_64-host\n",
            "manifest.any-not-alone",
            5,
        ),
        (
            "    language 1\n    targets x86_64-host, x86_64-host\n",
            "manifest.duplicate",
            5,
        ),
        (
            "    language 1\n    targets x86_64-nanos\n",
            "manifest.reserved-target",
            5,
        ),
        (
            "    language 1\n    targets x86_64-arm\n",
            "manifest.unknown-target",
            5,
        ),
        (
            "    language 2\n    targets any\n",
            "language.unsupported",
            4,
        ),
        (
            "    language 1u8\n    targets any\n",
            "language.version-form",
            4,
        ),
        (
            "    language 1\n    targets any\n    dep units \"units@1.2\"\n",
            "manifest.bad-source",
            6,
        ),
        (
            "    language 1\n    targets any\n    dep units \"/abs\"\n",
            "manifest.bad-source",
            6,
        ),
        (
            "    language 1\n    targets any\n    dep a \"../units\"\n    dep b \"./../units\"\n",
            "manifest.duplicate",
            7,
        ),
        (
            "    language 1\n    targets any\n    dep a \"nan.dev/units\"\n    dep b \"nan.dev/units\"\n",
            "manifest.duplicate",
            7,
        ),
        (
            "    language 1\n    targets any\n    allow device in app, app\n",
            "manifest.duplicate",
            6,
        ),
        (
            "    language 1\n    targets any\n    allow trusted in app\n    allow trusted in other\n",
            "manifest.duplicate",
            7,
        ),
        (
            "    language 1\n    targets any\n    deny app -> app\n",
            "manifest.deny-self",
            6,
        ),
        (
            "    language 1\n    targets any\n    deny a -> b\n    deny a -> b\n",
            "manifest.duplicate",
            7,
        ),
    ];
    for (directives, code, line) in cases {
        let (_scratch, workspace) = load(&[("app/package.nan", &manifest(directives))]);
        assert_eq!(
            codes(&workspace),
            [("app/package.nan".to_owned(), *line, *code)],
            "{directives}"
        );
    }
    let (_scratch, workspace) = load(&[(
        "app/package.nan",
        &manifest(
            "    language 1\n    targets any\n    dep a \"../../units\"\n    dep b \"../units\"\n",
        ),
    )]);
    assert_eq!(codes(&workspace), []);
}

#[test]
fn an_unsupported_language_leaves_the_other_files_unread() {
    let mut read = Vec::new();
    let scratch = Scratch::new(&[
        (
            "app/package.nan",
            &manifest("    language 2\n    targets any\n"),
        ),
        ("app/app.nan", "//! Root.\n"),
    ]);
    let workspace = Workspace::load(scratch.0.clone(), &mut read).expect("load the workspace");
    assert_eq!(read, ["app/package.nan"]);
    assert_eq!(workspace.files().count(), 1);
}

#[test]
fn manifests_stand_alone_in_package_nan() {
    let (_scratch, workspace) =
        load(&[("app/package.nan", "//! A package.\n\nconst X: u32 = 1\n")]);
    assert_eq!(
        codes(&workspace),
        [("app/package.nan".to_owned(), 1, "manifest.missing")]
    );
    let (_scratch, workspace) = load(&[
        (
            "app/package.nan",
            &format!("{MANIFEST}\nconst X: u32 = 1\n"),
        ),
        (
            "app/app.nan",
            "//! Root.\n\npackage other {\n    language 1\n    targets any\n} other\n",
        ),
    ]);
    assert_eq!(
        codes(&workspace),
        [
            ("app/package.nan".to_owned(), 8, "manifest.misplaced"),
            ("app/app.nan".to_owned(), 3, "manifest.misplaced")
        ]
    );
}

#[test]
fn modules_hold_their_summary_files() {
    let (_scratch, workspace) = load(&[
        ("app/package.nan", MANIFEST),
        ("app/other.nan", "//! Not the root summary.\n"),
        ("app/deep/units/units.nan", "//! Units.\n"),
        ("app/deep/units/more.nan", "//! More units.\n"),
        ("app/net/socket.nan", "//! Sockets.\n"),
    ]);
    let mut found = codes(&workspace);
    found.sort();
    assert_eq!(
        found,
        [
            (
                "app/net/socket.nan".to_owned(),
                1,
                "doc.module-file-missing"
            ),
            ("app/package.nan".to_owned(), 3, "doc.module-file-missing")
        ]
    );
    let package = &workspace.packages[0];
    let names: Vec<&str> = package
        .modules
        .iter()
        .map(|module| module.name.as_str())
        .collect();
    assert_eq!(names, ["units", "net", "app"]);
}

#[test]
fn variants_cover_each_declared_target_once() {
    let targets = manifest("    language 1\n    targets x86_64-bare, x86_64-host\n");
    let base = [
        ("app/package.nan", targets.as_str()),
        ("app/app.nan", "//! Root.\n"),
    ];
    let case = |variants: &[&str]| {
        let mut files: Vec<(String, String)> = base
            .iter()
            .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
            .collect();
        for variant in variants {
            files.push((
                format!("app/{variant}/io.nan"),
                "//! Input and output.\n".to_owned(),
            ));
        }
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str()))
            .collect();
        let (_scratch, workspace) = load(&files);
        codes(&workspace)
    };
    assert_eq!(case(&["io@bare", "io@host"]), []);
    assert_eq!(
        case(&["io@bare"]),
        [("app/package.nan".to_owned(), 5, "module.variant-missing")]
    );
    assert_eq!(
        case(&["io@bare", "io@host", "io@x86_64"]),
        [(
            "app/io@x86_64/io.nan".to_owned(),
            1,
            "module.variant-overlap"
        )]
    );
    assert_eq!(
        case(&["io@bare", "io@host", "io@uefi"]),
        [(
            "app/io@uefi/io.nan".to_owned(),
            1,
            "module.variant-unmatched"
        )]
    );
    assert_eq!(
        case(&["io", "io@bare", "io@host"]),
        [
            ("app/io@bare/io.nan".to_owned(), 1, "module.variant-mixed"),
            ("app/io@host/io.nan".to_owned(), 1, "module.variant-mixed"),
        ]
    );
}

#[test]
fn symbolic_links_in_a_package_are_reported_and_not_followed() {
    let scratch = Scratch::new(&[
        ("app/package.nan", MANIFEST),
        ("app/app.nan", "//! Root.\n"),
        ("elsewhere/x.nan", "//! X.\n"),
    ]);
    std::os::unix::fs::symlink(scratch.0.join("elsewhere"), scratch.0.join("app/linked"))
        .expect("link a directory");
    std::os::unix::fs::symlink(scratch.0.join("elsewhere"), scratch.0.join("app/.hidden"))
        .expect("link a hidden directory");
    let workspace =
        Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("load the workspace");
    let links: Vec<(String, Vec<String>)> = workspace
        .diagnostics()
        .filter(|diagnostic| diagnostic.code == Code::BuildSymbolicLink)
        .map(|diagnostic| {
            (
                diagnostic.path.unwrap_or_default(),
                diagnostic
                    .because
                    .iter()
                    .map(|reason| reason.subject.clone())
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        links,
        [("app/package.nan".to_owned(), vec!["linked".to_owned()])]
    );
    assert!(
        workspace
            .files()
            .all(|(_, file)| !file.path.contains("linked"))
    );
}

#[test]
fn items_have_canonical_paths() {
    let (_scratch, workspace) = load(&[
        ("app/package.nan", MANIFEST),
        (
            "app/app.nan",
            "//! Root.\n\npub closed struct Gauge {\n    pub level: u32\n} Gauge\n\nfn Gauge.raise(mut self) {\n} Gauge.raise\n\nfn units::Scale.half(self) = 1\n",
        ),
        (
            "app/units/units.nan",
            "//! Units.\n\npub closed enum Scale {\n    Base\n} Scale\n",
        ),
    ]);
    let at = |path: &str, line: u32| workspace.item_at(path, line).map(|item| item.canonical());
    assert_eq!(at("app/package.nan", 4).as_deref(), Some("app"));
    assert_eq!(at("app/app.nan", 4).as_deref(), Some("app::Gauge"));
    assert_eq!(at("app/app.nan", 7).as_deref(), Some("app::Gauge.raise"));
    assert_eq!(
        at("app/app.nan", 10).as_deref(),
        Some("app::units::Scale.half")
    );
    assert_eq!(
        at("app/units/units.nan", 4).as_deref(),
        Some("app::units::Scale")
    );
}

#[test]
fn two_sources_of_one_package_name_split_it() {
    let units = "//! Units.\n\npackage units {\n    language 1\n    targets any\n} units\n";
    let (_scratch, workspace) = load(&[
        ("units/package.nan", units),
        ("copy/package.nan", units),
        (
            "app/package.nan",
            &manifest(
                "    language 1\n    targets any\n    dep a \"../units\"\n    dep b \"../copy\"\n    dep c \"./../units\"\n",
            ),
        ),
    ]);
    let split: Vec<u32> = workspace
        .diagnostics()
        .filter(|diagnostic| diagnostic.code.name() == "dep.split-package")
        .filter_map(|diagnostic| diagnostic.line)
        .collect();
    assert_eq!(split, [7]);
}
