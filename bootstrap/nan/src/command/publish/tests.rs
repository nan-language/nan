use std::fs;

use crate::command::key::tests::TestPrincipal;
use crate::command::tests::run_in;
use crate::workspace::tests::Scratch;

const FIRST: &str = "//! Modes.\n\n/// A mode.\npub open enum Mode derives Eq, Encode {\n    Idle\n    Busy\n} Mode\n\n/// The level of a mode.\npub fn Mode.level(self) -> u8 {\n    return match self {\n        Mode.Idle => 0\n        Mode.Busy => 2\n    }\n} Mode.level\n";

const SECOND: &str = "//! Modes.\n\n/// A mode.\npub open enum Mode derives Eq, Encode {\n    Idle\n    Busy\n    Draining\n} Mode\n\n/// The level of a mode.\npub fn Mode.level(self) -> u8 {\n    return match self {\n        Mode.Idle => 0\n        Mode.Busy => 2\n        Mode.Draining => 1\n    }\n} Mode.level\n\n/// Whether a mode is idle.\npub fn Mode.is_idle(self) -> bool = self == Mode.Idle\n";

/// The release and encode classes a publish prints.
fn classes(output: &str) -> String {
    let line = output.lines().find(|line| line.starts_with("published "));
    let fields: Vec<&str> = line.expect("a published release").split(' ').collect();
    fields[3..].join(" ")
}

#[test]
fn a_new_variant_of_an_open_enum_is_additive_and_a_removed_one_breaking() {
    let scratch = Scratch::new(&[(
        "modes/package.nan",
        "//! Modes.\n\npackage modes {\n    language 1\n    targets any\n} modes\n",
    )]);
    let dir = &scratch.0;
    let source = dir.join("modes/modes.nan");
    let name = format!(
        "nan-test/{}",
        dir.file_name().expect("a directory name").to_string_lossy()
    );
    fs::write(&source, FIRST).expect("the first release");
    run_in(dir, "api update");
    run_in(dir, &format!("publish --local modes --name {name}"));
    fs::write(&source, SECOND).expect("the second release");
    run_in(dir, "api update --confirm-docs modes::Mode");
    let second = run_in(dir, "publish --local modes");
    assert_eq!(
        classes(&second.output),
        "additive additive",
        "{}",
        second.output
    );
    let third = SECOND
        .replace("    Busy\n    Draining\n", "    Draining\n")
        .replace("        Mode.Busy => 2\n", "");
    fs::write(&source, third).expect("the third release");
    run_in(dir, "api update --confirm-docs modes::Mode");
    let third = run_in(dir, "publish --local modes");
    assert_eq!(
        classes(&third.output),
        "breaking breaking",
        "{}",
        third.output
    );
}

#[test]
fn a_release_published_as_a_principal_is_read_only_from_its_pinned_and_trusted_key_until_forged() {
    let scratch = Scratch::new(&[(
        "procinfo/package.nan",
        "//! Process facts.\n\npackage procinfo {\n    language 1\n    targets x86_64-host\n    allow trusted in procinfo\n} procinfo\n",
    )]);
    let dir = &scratch.0;
    let scratch_name = dir.file_name().expect("a directory name").to_string_lossy();
    let name = format!("nan-test/{scratch_name}");
    fs::write(
        dir.join("procinfo/procinfo.nan"),
        format!("//! Reads process facts.\n\n/// The process id, as {scratch_name} reads it.\npub foreign fn getpid() -> i32 limits notrap, terminates, noalloc convention \"sysv\"\n"),
    )
    .expect("the procinfo source");
    let publisher = TestPrincipal::new("publisher", Some([4; 32]));
    let author = TestPrincipal::new("forgeable-author", None);
    run_in(dir, "api update");
    run_in(dir, &format!("seal procinfo::getpid --as {}", publisher.0));
    let own = run_in(
        dir,
        &format!(
            "publish --local procinfo --name {name} --as {}",
            publisher.0
        ),
    )
    .output;
    assert!(
        own.contains("\nincomplete seal.same-author procinfo/procinfo.nan:4 "),
        "{own}"
    );
    let published = run_in(
        dir,
        &format!(
            "publish --local procinfo --name {name} --as {} --author {}",
            publisher.0, author.0
        ),
    )
    .output;
    let root = published
        .lines()
        .find_map(|line| line.strip_prefix("published "))
        .and_then(|line| line.split(' ').nth(1))
        .expect("a release")[..14]
        .to_owned();
    fs::create_dir_all(dir.join("app")).expect("the app directory");
    fs::write(
        dir.join("app/package.nan"),
        format!("//! Apps.\n\npackage app {{\n    language 1\n    targets x86_64-host\n    dep procinfo \"{name}\"\n    allow trusted in procinfo\n}} app\n"),
    )
    .expect("the app manifest");
    fs::write(
        dir.join("app/app.nan"),
        "//! Apps.\n\npub fn id() -> i32 {\n    return procinfo::getpid()\n} id\n",
    )
    .expect("the app source");
    let app = dir.join("app");
    run_in(&app, "dep sync");
    let key = TestPrincipal::key_of([4; 32]);
    let lock = fs::read_to_string(app.join(".nan/lock.nan")).expect("the app's lock");
    assert!(lock.contains(&format!(" signed \"{key}\" ")), "{lock}");
    let untrusted = run_in(dir, "check app").output;
    assert!(
        untrusted.contains("\nerror artefact.bad-signature app/package.nan:6 app\n"),
        "{untrusted}"
    );
    run_in(&app, &format!("key trust {} {key}", publisher.0));
    let checked = run_in(dir, "check app").output;
    assert!(checked.starts_with("ok check app"), "{published}{checked}");
    let store = std::env::temp_dir().join("nan-test-home/.nan/store");
    let entry = fs::read_dir(&store)
        .expect("the store")
        .map(|entry| entry.expect("a store entry").path())
        .find(|path| {
            path.file_name()
                .and_then(|file| file.to_str()?.split_once('-'))
                .is_some_and(|(_, held)| held.starts_with(&root))
        })
        .expect("the release's entry");
    let mut file = fs::read(&entry).expect("the entry");
    let held = file.len() - 32;
    let at = file[..held]
        .windows(author.0.len())
        .position(|window| window == author.0.as_bytes())
        .expect("the author in the provenance");
    file[at] ^= 1;
    let digest = crate::hash::blake3(&file[..held]);
    file[held..].copy_from_slice(&digest);
    fs::write(&entry, file).expect("the forged entry");
    let forged = run_in(dir, "check app").output;
    assert!(
        forged.contains("\nerror artefact.bad-signature app/package.nan:6 app\n"),
        "{forged}"
    );
}

#[test]
fn a_release_is_built_only_from_the_sources_its_attestation_hashes_and_that_check_whole() {
    let scratch = Scratch::new(&[(
        "meter/package.nan",
        "//! Meters.\n\npackage meter {\n    language 1\n    targets any\n} meter\n",
    )]);
    let dir = &scratch.0;
    let scratch_name = dir.file_name().expect("a directory name").to_string_lossy();
    let name = format!("nan-test/{scratch_name}");
    fs::write(
        dir.join("meter/meter.nan"),
        format!("//! Meters.\n\n/// The level of {scratch_name}.\npub fn level() -> u32\n    example level() == 1\n{{\n    return 1\n}} level\n"),
    )
    .expect("the meter source");
    run_in(dir, "api update");
    run_in(dir, &format!("publish --local meter --name {name}"));
    fs::create_dir_all(dir.join("app")).expect("the app directory");
    fs::write(
        dir.join("app/package.nan"),
        format!("//! Apps.\n\npackage app {{\n    language 1\n    targets x86_64-host\n    entry main\n    dep meter \"{name}\"\n}} app\n"),
    )
    .expect("the app manifest");
    fs::write(
        dir.join("app/app.nan"),
        "//! Apps.\n\nfn main(take world: host::World) fails task::Cancelled uses wait {\n    world.log.info(\"level\", value: meter::level())\n} main\n",
    )
    .expect("the app source");
    run_in(&dir.join("app"), "dep sync");
    let cache = crate::cache::Cache::open()
        .expect("the user directory")
        .unwrap_or_else(|_| panic!("an exposed user directory"));
    let entry = crate::store::Store::new(cache.store())
        .entries()
        .expect("the store")
        .unwrap_or_else(|_| panic!("an exposed store"))
        .into_iter()
        .find(|entry| entry.name.as_deref() == Some(name.as_str()))
        .expect("the release's entry");
    let path = fs::read_dir(std::env::temp_dir().join("nan-test-home/.nan/store"))
        .expect("the store directory")
        .map(|file| file.expect("a store file").path())
        .find(|path| {
            path.file_name()
                .and_then(|file| file.to_str()?.split_once('-'))
                .is_some_and(|(index, _)| index == entry.index.to_string())
        })
        .expect("the entry's file");
    let forged = entry
        .sources
        .iter()
        .map(|(path, content)| {
            let content = String::from_utf8_lossy(content).replace("return 1", "return 2");
            (path.clone(), content.into_bytes())
        })
        .collect::<Vec<_>>();
    let mut file = fs::read(&path).expect("the entry");
    let replace = |file: &mut Vec<u8>, from: &[u8], to: &[u8]| {
        let at = file
            .windows(from.len())
            .rposition(|window| window == from)
            .expect("the replaced bytes");
        file[at..at + to.len()].copy_from_slice(to);
        let held = file.len() - 32;
        let digest = crate::hash::blake3(&file[..held]);
        file[held..].copy_from_slice(&digest);
    };
    replace(&mut file, b"return 1", b"return 2");
    fs::write(&path, &file).expect("the entry with forged sources");
    let unsourced = run_in(dir, "check app").output;
    assert!(
        unsourced.contains("\nerror artefact.source-mismatch app/package.nan:7 app\n"),
        "{unsourced}"
    );
    let attested = crate::artefact::source_tree_hash(&entry.sources);
    let forged = crate::artefact::source_tree_hash(&forged);
    replace(&mut file, &attested.0, &forged.0);
    fs::write(&path, &file).expect("the entry with a forged attestation");
    let refused = format!(
        "\nerror artefact.recheck-failed app/package.nan:7 app\n        dep meter \"{name}\"\n    because diagnostic meter/meter.nan:5 meter::level example.failed\n        example level() == 1\n"
    );
    for command in ["build app", "verify app"] {
        let output = run_in(dir, command).output;
        assert!(output.contains(&refused), "{output}");
    }
}
