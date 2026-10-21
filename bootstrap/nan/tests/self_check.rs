//! The self-check harness: builds the NaN front end of `toolchain/` once per state of its files
//! and of this harness, then compares what it prints for `nan check`, with its exit status, and
//! the canonical NIR it gives, with the bootstrap's: for every `nan` example of each specification
//! chapter, with each `nan check` command of its section's `text` blocks, for every std package,
//! and for the toolchain workspace, which must check without a diagnostic and pass `nan verify`.
//! The front end runs on a copy of the workspace and local store the bootstrap has just used,
//! on one processor under an address-space cap, with a user directory of its own.

use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use datatest_stable::Utf8Path;

mod common;

use common::{examples, nan};

datatest_stable::harness! {
    { test = chapter, root = "../../docs/spec", pattern = r"^\d\d-[a-z-]+\.md$" },
    { test = std_package, root = "../../std", pattern = r"^[a-z_]+/package\.nan$" },
    { test = toolchain, root = "../../toolchain", pattern = r"^nan/package\.nan$" },
}

/// The root file the front end is built with: `nan check` with the user directory and local
/// store the harness gives, or, when it asks for NIR, the canonical NIR stream of each package
/// of the workspace, or of the one it names, each after a line holding its name and length.
const DRIVER: &str = r#"//! The entry of the NaN front end under the self-check harness.

/// What the harness gives through the environment: the user directory and local store, and the
/// package whose NIR it asks for, empty for every package.
struct Harness {
    home: Option[cache::HomeOverride]
    nir: Option[text::String]
} Harness

fn main(take world: host::World) -> u8 fails task::Cancelled uses wait {
    let alloc = world.alloc
    let given = harness(world.environment, alloc)
    let Some(wanted) = given.nir else {
        return command::entry(take world, home: given.home)
    }
    let Ok(directory) = attempt platform::current_dir(alloc) else {
        return 101
    }
    let Ok(current) = attempt text::String.try_from_utf8(directory.view(), alloc) else {
        return 101
    }
    let root = workspace::root_of(current.as_str(), alloc)
    let grammar = syntax::Grammar.load(alloc)
    var read = mem::List[text::String].empty(alloc)
    let Ok(loaded) = attempt workspace::Workspace.load(root.as_str(), read: mut read, given: given.home, grammar, alloc) else {
        return 101
    }
    let std = workspace::Workspace.of_std(grammar, alloc)
    let session = check::Session.of(mem::Shared.of(take loaded, alloc: alloc), std: mem::Shared.of(take std, alloc: alloc), grammar: mem::Shared.of(take grammar, alloc: alloc), home: cache::copied_override(given.home, alloc), alloc)
    discard attempt platform::write_stdout(session.nir_streams(wanted.as_str()).view())
    return 0
} main

validator fn harness(raw: Untrusted[mem::Map[mem::Bytes, mem::Bytes]], alloc: mem::Alloc) -> Harness {
    let variables = raw.inspect()
    var given = Harness(home: None, nir: None)
    match variables.get_by(fn(key: mem::Bytes) => key.view().cmp(b"NAN_SELF_CHECK_NIR")) {
        Some(name) => {
            given.nir = Some(output::lossy(name.view(), alloc))
        }
        None => {}
    }
    let Some(home) = variables.get_by(fn(key: mem::Bytes) => key.view().cmp(b"NAN_SELF_CHECK_HOME")) else {
        return given
    }
    var store: Option[text::String] = None
    match variables.get_by(fn(key: mem::Bytes) => key.view().cmp(b"NAN_SELF_CHECK_STORE")) {
        Some(path) => {
            store = Some(output::lossy(path.view(), alloc))
        }
        None => {}
    }
    given.home = Some(cache::HomeOverride(home: output::lossy(home.view(), alloc), store: take store))
    return given
} harness
"#;

/// The grant of the toolchain's manifest that the driver widens to its root module.
const GRANT: &str = "allow validate in platform, command\n";

/// How long one run of the front end may take.
const RUN_LIMIT: Duration = Duration::from_secs(900);

/// The repository's root.
fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The harness's directory under the harnesses' temporary directory.
fn base() -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("self-check")
}

/// Makes this process use the harnesses' home directory and the local store named `store`,
/// emptied, and gives that store's directory.
fn fresh_store(store: &str) -> std::io::Result<PathBuf> {
    let tmp = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let dir = tmp.join("stores").join(store);
    match fs::remove_dir_all(&dir) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    fs::create_dir_all(tmp.join("home"))?;
    fs::create_dir_all(tmp.join("stores"))?;
    nan::use_home(tmp.join("home"), Some(dir.clone()));
    Ok(dir)
}

/// Copies a directory tree into another, made anew, without the `.nan` directory at its top
/// when `machine_root` is false.
fn copy_tree(from: &Path, to: &Path, machine_root: bool) -> std::io::Result<()> {
    if to.exists() {
        fs::remove_dir_all(to)?;
    }
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        if !machine_root && entry.file_name() == ".nan" {
            continue;
        }
        let target = to.join(entry.file_name());
        match entry.file_type()?.is_dir() {
            true => copy_tree(&entry.path(), &target, true)?,
            false => drop(fs::copy(entry.path(), target)?),
        }
    }
    Ok(())
}

/// Feeds every file under `dir` but its top `.nan` directory, by path, to `hasher`.
fn hash_tree(dir: &Path, prefix: &str, hasher: &mut DefaultHasher) -> std::io::Result<()> {
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if prefix.is_empty() && name == ".nan" {
            continue;
        }
        let path = format!("{prefix}{name}");
        if entry.file_type()?.is_dir() {
            hash_tree(&entry.path(), &format!("{path}/"), hasher)?;
        } else {
            path.hash(hasher);
            fs::read(entry.path())?.hash(hasher);
        }
    }
    Ok(())
}

/// The front end image for the current state of `toolchain/`, this harness and the bootstrap,
/// built by the first harness process needing it while the others wait.
fn image() -> Result<PathBuf, String> {
    let fault = |error: std::io::Error| error.to_string();
    let source = repository().join("toolchain");
    let mut hasher = DefaultHasher::new();
    hash_tree(&source, "", &mut hasher).map_err(fault)?;
    DRIVER.hash(&mut hasher);
    let harness = fs::metadata(std::env::current_exe().map_err(fault)?).map_err(fault)?;
    (harness.len(), harness.modified().map_err(fault)?).hash(&mut hasher);
    let image = base()
        .join("images")
        .join(format!("{:016x}", hasher.finish()));
    fs::create_dir_all(base().join("images")).map_err(fault)?;
    let lock = fs::File::create(base().join("build.lock")).map_err(fault)?;
    lock.lock().map_err(fault)?;
    if image.exists() {
        return Ok(image);
    }
    let build = base().join("build");
    copy_tree(&source, &build, false).map_err(fault)?;
    let manifest = fs::read_to_string(build.join("nan/package.nan")).map_err(fault)?;
    if !manifest.contains(GRANT) {
        return Err(format!("the toolchain's manifest lacks `{}`", GRANT.trim()));
    }
    let widened = manifest.replace(GRANT, "allow validate in platform, command, nan\n");
    fs::write(build.join("nan/package.nan"), widened).map_err(fault)?;
    fs::write(build.join("nan/nan.nan"), DRIVER).map_err(fault)?;
    let (output, status) = nan(&build, &["build", "nan"])?;
    if status != Some(0) {
        return Err(format!("the front end does not build:\n{output}"));
    }
    fs::copy(build.join(".nan/images/x86_64-host/nan"), &image).map_err(fault)?;
    Ok(image)
}

/// One processor of the harness's affinity set, picked by its process id. The runtime of an
/// image sets up an executor thread and an io_uring instance per processor of its affinity set,
/// and the kernel charges the instances' memory to the user's locked-memory limit, which
/// concurrent front end runs with every processor would exhaust.
fn processor() -> String {
    let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
    let allowed: Vec<u32> = status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
        .unwrap_or("0")
        .trim()
        .split(',')
        .flat_map(|range| {
            let (low, high) = range.split_once('-').unwrap_or((range, range));
            low.parse().unwrap_or(0)..=high.parse().unwrap_or(0)
        })
        .collect();
    allowed[std::process::id() as usize % allowed.len()].to_string()
}

/// What a run of the front end in `dir` prints and the status it exits with: with the
/// harness's user directory and the local store `store`, on one processor, under a 6 GB
/// address-space cap, killed after `RUN_LIMIT`; with `nir`, the NIR of the package it names,
/// every one when empty.
fn front(
    image: &Path,
    dir: &Path,
    store: &Path,
    nir: Option<&str>,
    args: &[&str],
) -> Result<(Vec<u8>, Option<i32>), String> {
    let home = base().join("home");
    fs::create_dir_all(&home).map_err(|error| error.to_string())?;
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg("ulimit -v 6000000; exec taskset -c \"$0\" \"$@\"")
        .arg(processor())
        .arg(image)
        .args(args)
        .current_dir(dir)
        .env("NAN_SELF_CHECK_HOME", &home)
        .env("NAN_SELF_CHECK_STORE", store)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(package) = nir {
        command.env("NAN_SELF_CHECK_NIR", package);
    }
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let mut stdout = child.stdout.take().expect("the front end's output");
    let mut stderr = child.stderr.take().expect("the front end's diagnostics");
    let printed = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        drop(stdout.read_to_end(&mut bytes));
        bytes
    });
    let reported = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        drop(stderr.read_to_end(&mut bytes));
        bytes
    });
    let deadline = Instant::now() + RUN_LIMIT;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            drop(child.kill());
            drop(child.wait());
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let printed = printed.join().expect("the front end's output read");
    let reported = reported.join().expect("the front end's diagnostics read");
    let Some(status) = status else {
        return Err(format!(
            "the front end runs over {RUN_LIMIT:?}: nan {}",
            args.join(" ")
        ));
    };
    if !reported.is_empty() {
        return Err(format!(
            "the front end exits with {:?} on nan {} and reports\n{}",
            status.code(),
            args.join(" "),
            String::from_utf8_lossy(&reported)
        ));
    }
    Ok((printed, status.code()))
}

/// Copies the workspace and local store the bootstrap uses into the front end's, then compares
/// what both print for `nan <args>` and the status they exit with. Gives what they print.
fn compare_check(
    image: &Path,
    bootstrap: &Path,
    store: &Path,
    args: &[&str],
) -> Result<String, String> {
    let fault = |error: std::io::Error| error.to_string();
    let (front_dir, front_store) = fronts(bootstrap, store);
    copy_tree(bootstrap, &front_dir, true).map_err(fault)?;
    match store.exists() {
        true => copy_tree(store, &front_store, true).map_err(fault)?,
        false => drop(fs::remove_dir_all(&front_store)),
    }
    let (expected, status) = nan(bootstrap, args)?;
    let (printed, code) = front(image, &front_dir, &front_store, None, args)?;
    if printed != expected.as_bytes() || code != status {
        return Err(format!(
            "nan {} exits with {status:?} and prints\n{expected}the front end exits with {code:?} and prints\n{}",
            args.join(" "),
            String::from_utf8_lossy(&printed)
        ));
    }
    Ok(expected)
}

/// The workspace and local store directories of the front end's runs beside the bootstrap's.
fn fronts(bootstrap: &Path, store: &Path) -> (PathBuf, PathBuf) {
    let mut front_dir = bootstrap.as_os_str().to_owned();
    front_dir.push("-front");
    let mut front_store = store.as_os_str().to_owned();
    front_store.push("-front");
    (PathBuf::from(front_dir), PathBuf::from(front_store))
}

/// Compares the canonical NIR stream of each package of the workspace, or of the one `package`
/// names, with the front end's, run on its copy of the workspace.
fn compare_nir(
    image: &Path,
    bootstrap: &Path,
    store: &Path,
    package: Option<&str>,
) -> Result<(), String> {
    let streams = nan::nir_streams(bootstrap).map_err(|error| error.to_string())?;
    let mut expected = Vec::new();
    for (name, stream) in &streams {
        if package.is_some_and(|package| package != name) {
            continue;
        }
        expected.extend(format!("{name} {}\n", stream.len()).bytes());
        expected.extend(stream);
    }
    let (front_dir, front_store) = fronts(bootstrap, store);
    let (printed, code) = front(
        image,
        &front_dir,
        &front_store,
        Some(package.unwrap_or_default()),
        &[],
    )?;
    if printed != expected || code != Some(0) {
        return Err(format!(
            "the front end exits with {code:?} and gives NIR differing from the bootstrap's at byte {}",
            printed
                .iter()
                .zip(&expected)
                .position(|(given, wanted)| given != wanted)
                .unwrap_or(printed.len().min(expected.len()))
        ));
    }
    Ok(())
}

fn chapter(path: &Utf8Path, text: String) -> datatest_stable::Result<()> {
    let chapter = path.file_stem().expect("a chapter file");
    let store = fresh_store(&format!("self-check-{chapter}"))?;
    let image = image()?;
    let mut failures = Vec::new();
    for example in examples(&text) {
        match fs::remove_dir_all(&store) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
            _ => {}
        }
        let dir = base()
            .join("chapters")
            .join(chapter)
            .join(example.line.to_string());
        let compared = (|| {
            example
                .write(&dir, false)
                .map_err(|error| error.to_string())?;
            example.prepare(&dir)?;
            compare_check(&image, &dir, &store, &["check"])?;
            compare_nir(&image, &dir, &store, None)?;
            for (command, _) in &example.commands {
                let args: Vec<&str> = command.split(' ').skip(1).collect();
                match args.first() {
                    Some(&"check") => drop(compare_check(&image, &dir, &store, &args)?),
                    _ => drop(nan(&dir, &args)?),
                }
            }
            Ok::<(), String>(())
        })();
        match compared {
            Ok(()) => {
                fs::remove_dir_all(&dir)?;
                fs::remove_dir_all(fronts(&dir, &store).0)?;
            }
            Err(failure) => failures.push(format!("{path}:{}: {failure}", example.line)),
        }
    }
    match failures.is_empty() {
        true => Ok(()),
        false => Err(failures.join("\n\n").into()),
    }
}

fn std_package(path: &Utf8Path, _: String) -> datatest_stable::Result<()> {
    let package = path
        .parent()
        .and_then(Utf8Path::file_name)
        .expect("a package directory");
    let store = fresh_store(&format!("self-check-std-{package}"))?;
    let image = image()?;
    let dir = base().join("std").join(package);
    copy_tree(&repository().join("std"), &dir, false)?;
    compare_check(&image, &dir, &store, &["check", package])?;
    compare_nir(&image, &dir, &store, Some(package))?;
    fs::remove_dir_all(&dir)?;
    fs::remove_dir_all(fronts(&dir, &store).0)?;
    Ok(())
}

/// Checks the toolchain workspace with both, which must agree and report no diagnostic, and
/// verifies it, in a copy outside every git working tree.
fn toolchain(_: &Utf8Path, _: String) -> datatest_stable::Result<()> {
    let store = fresh_store("self-check-toolchain")?;
    let image = image()?;
    let outside = std::env::temp_dir().join(format!("nan-self-check-{}", std::process::id()));
    let dir = outside.join("toolchain");
    copy_tree(&repository().join("toolchain"), &dir, false)?;
    let output = compare_check(&image, &dir, &store, &["check"])?;
    if output.lines().any(|line| !line.starts_with("ok check ")) {
        return Err(format!("the toolchain workspace checks with diagnostics:\n{output}").into());
    }
    compare_nir(&image, &dir, &store, None)?;
    let (output, _) = nan(&dir, &["verify"])?;
    if !output.starts_with("ok ") {
        return Err(format!("nan verify reports\n{output}").into());
    }
    fs::remove_dir_all(outside)?;
    Ok(())
}
