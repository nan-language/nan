use crate::command::tests::run_in;
use crate::workspace::tests::Scratch;

const MANIFEST: &str =
    "//! Example.\n\npackage example {\n    language 1\n    targets any\n} example\n";

const ACCEPTED: &str = "//! Example.\n\npub fn clamp_level(level: u32, ceiling: u32) -> u32\n    ensures result <= ceiling\n{\n    if level > ceiling {\n        return ceiling\n    }\n    return level\n} clamp_level\n";

#[test]
fn a_weakened_function_is_in_the_interface_spec_and_trust_classes() {
    let weakened = ACCEPTED.replace(
        "    ensures result <= ceiling\n",
        "    requires ceiling > 0\n    ensures result <= ceiling\n",
    );
    let scratch = Scratch::new(&[
        ("old/example/package.nan", MANIFEST),
        ("old/example/example.nan", ACCEPTED),
        ("new/example/package.nan", MANIFEST),
        ("new/example/example.nan", &weakened),
    ]);
    run_in(&scratch.0.join("old"), "api update");
    let api = scratch.0.join("old/example/.nan/api/example.nan");
    let new_api = scratch.0.join("new/example/.nan/api/example.nan");
    std::fs::create_dir_all(new_api.parent().expect("a directory")).expect("the api directory");
    std::fs::copy(api, new_api).expect("the accepted snapshot");
    run_in(&scratch.0.join("new"), "api update");
    let run = run_in(&scratch.0, "diff old new");
    assert!(
        run.output.contains(
            "\nchanged example::clamp_level example/example.nan:3 interface spec trust weakening\n"
        ),
        "{}",
        run.output
    );
    assert!(run.output.contains("\n    +     requires ceiling > 0\n"));
}
