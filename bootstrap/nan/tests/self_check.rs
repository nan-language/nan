//! The self-check harness: builds the NaN toolchain of `toolchain/` once per state of its files
//! and of this harness, with a root file of its own, then runs it beside the bootstrap and
//! compares what both print, their exit statuses and the files they write: for every `nan`
//! example of each specification chapter, the commands the conformance harness runs and those of
//! its section's `text` blocks, with the canonical NIR, the text of each function a final build
//! lowers and what each `x86_64-host` program writes; for every std package, `nan check` with its
//! NIR and `nan test`; for every program of `tests/programs`, its final build and lowering and
//! what it writes; and for the toolchain workspace, which must check without a diagnostic and
//! pass `nan verify` with both. The toolchain test then checks the self-built toolchain by diverse
//! double-compiling: the toolchain the bootstrap built builds the toolchain, which builds it again
//! into the same bytes, and the bootstrap's stage comparison accepts it. The NaN toolchain runs on
//! a copy of the workspace and local store the bootstrap has just used, on one processor under an
//! address-space cap, with a user directory of its own.

use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use datatest_stable::Utf8Path;

mod common;

use common::{Example, examples, nan, unmark};

datatest_stable::harness! {
    { test = chapter, root = "../../docs/spec", pattern = r"^\d\d-[a-z-]+\.md$" },
    { test = std_package, root = "../../std", pattern = r"^[a-z_]+/package\.nan$" },
    { test = program, root = "tests/programs", pattern = r"^[a-z-]+/expected$" },
    { test = toolchain, root = "../../toolchain", pattern = r"^nan/package\.nan$" },
}

/// The root file the toolchain is built with: the command line with the user directory and local
/// store the harness gives, or, when it asks for NIR, the canonical NIR stream of each package of
/// the workspace, or of the one it names, each after a line holding its name and length, or, when
/// it asks for a program's lowering, each function its final build lowers, its text after a line
/// holding its symbol name and the text's length.
const DRIVER: &str = r#"//! The entry of the NaN toolchain under the self-check harness.

/// What the harness gives through the environment: the user directory and local store, the
/// package whose NIR it asks for, empty for every package, and the program whose lowering it asks
/// for.
struct Harness {
    home: Option[cache::HomeOverride]
    nir: Option[text::String]
    lowered: Option[text::String]
} Harness

fn main(take world: host::World) -> u8 fails task::Cancelled uses wait {
    let alloc = world.alloc
    let given = harness(world.environment, alloc)
    if given.nir.is_none() and given.lowered.is_none() {
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
    match given.nir {
        Some(wanted) => {
            discard attempt platform::write_stdout(session.nir_streams(wanted.as_str()).view())
        }
        None => {}
    }
    let Some(program) = given.lowered else {
        return 0
    }
    discard attempt platform::write_stdout(final_build::lowered_functions(session, package_name: program.as_str(), alloc).view())
    return 0
} main

validator fn harness(raw: Untrusted[mem::Map[mem::Bytes, mem::Bytes]], alloc: mem::Alloc) -> Harness {
    let variables = raw.inspect()
    var given = Harness(home: None, nir: None, lowered: None)
    match variables.get_by(fn(key: mem::Bytes) => key.view().cmp(b"NAN_SELF_CHECK_NIR")) {
        Some(name) => {
            given.nir = Some(output::lossy(name.view(), alloc))
        }
        None => {}
    }
    match variables.get_by(fn(key: mem::Bytes) => key.view().cmp(b"NAN_SELF_CHECK_LOWERED")) {
        Some(name) => {
            given.lowered = Some(output::lossy(name.view(), alloc))
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

/// How long one run of a program or of the NaN toolchain may take, and one of the NaN toolchain
/// over its own workspace, which checks, tests or builds the whole toolchain.
const RUN_LIMIT: Duration = Duration::from_secs(900);
const SELF_LIMIT: Duration = Duration::from_secs(5_400);

/// The address space, in KiB, a run of the NaN toolchain may take, and one over its own workspace.
const RUN_SPACE: u64 = 12_000_000;
const SELF_SPACE: u64 = 24_000_000;

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

/// Copies the toolchain workspace into `dir`, its root file replaced by the driver.
fn driven_copy(dir: &Path) -> Result<(), String> {
    let fault = |error: std::io::Error| error.to_string();
    copy_tree(&repository().join("toolchain"), dir, false).map_err(fault)?;
    let manifest = fs::read_to_string(dir.join("nan/package.nan")).map_err(fault)?;
    if !manifest.contains(GRANT) {
        return Err(format!("the toolchain's manifest lacks `{}`", GRANT.trim()));
    }
    let widened = manifest.replace(GRANT, "allow validate in platform, command, nan\n");
    fs::write(dir.join("nan/package.nan"), widened).map_err(fault)?;
    fs::write(dir.join("nan/nan.nan"), DRIVER).map_err(fault)
}

/// The toolchain image the bootstrap builds for the current state of `toolchain/`, this harness
/// and the bootstrap, built by the first harness process needing it while the others wait.
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
    driven_copy(&build)?;
    let (output, status) = nan(&build, &["build", "nan"])?;
    if status != Some(0) {
        return Err(format!("the toolchain does not build:\n{output}"));
    }
    fs::copy(build.join(".nan/images/x86_64-host/nan"), &image).map_err(fault)?;
    Ok(image)
}

/// One processor of the harness's affinity set, picked by its process id. The runtime of an
/// image sets up an executor thread and an io_uring instance per processor of its affinity set,
/// and the kernel charges the instances' memory to the user's locked-memory limit, which
/// concurrent toolchain runs with every processor would exhaust.
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

/// A NaN toolchain image and the bounds of each of its runs.
struct Toolchain {
    image: PathBuf,
    space: u64,
    limit: Duration,
}

impl Toolchain {
    fn new(image: PathBuf) -> Toolchain {
        Toolchain {
            image,
            space: RUN_SPACE,
            limit: RUN_LIMIT,
        }
    }

    /// The image bounded for runs over its own workspace.
    fn over_itself(image: PathBuf) -> Toolchain {
        Toolchain {
            image,
            space: SELF_SPACE,
            limit: SELF_LIMIT,
        }
    }
}

/// What the harness gives a run of the NaN toolchain.
struct Given<'a> {
    /// The user directory and the local store.
    home: &'a Path,
    store: &'a Path,
    /// The variables asking for NIR or a lowering, with their values.
    asked: &'a [(&'a str, &'a str)],
}

/// What a run of a NaN toolchain in `dir` prints and the status it exits with, on one processor,
/// under its bounds, killed after its time limit.
fn front(
    toolchain: &Toolchain,
    dir: &Path,
    given: &Given,
    args: &[&str],
) -> Result<(Vec<u8>, Option<i32>), String> {
    fs::create_dir_all(given.home).map_err(|error| error.to_string())?;
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(format!(
            "ulimit -v {}; exec taskset -c \"$0\" \"$@\"",
            toolchain.space
        ))
        .arg(processor())
        .arg(&toolchain.image)
        .args(args)
        .current_dir(dir)
        .env("NAN_SELF_CHECK_HOME", given.home)
        .env("NAN_SELF_CHECK_STORE", given.store)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (variable, value) in given.asked {
        command.env(variable, value);
    }
    let ran = supervised(command, toolchain.limit)?;
    let Some(status) = ran.status else {
        return Err(format!(
            "the NaN toolchain runs over {:?}: nan {}",
            toolchain.limit,
            args.join(" ")
        ));
    };
    if !ran.stderr.is_empty() {
        return Err(format!(
            "the NaN toolchain exits with {status:?} on nan {} and reports\n{}",
            args.join(" "),
            String::from_utf8_lossy(&ran.stderr)
        ));
    }
    Ok((ran.stdout, status))
}

/// What a supervised command writes to its standard output and error, and its exit status,
/// none when it was killed.
#[derive(Debug, PartialEq)]
struct Ran {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    status: Option<Option<i32>>,
}

/// Runs a command to its end or until `limit` passes, then kills it.
fn supervised(mut command: Command, limit: Duration) -> Result<Ran, String> {
    let mut child = command.spawn().map_err(|error| error.to_string())?;
    let mut stdout = child.stdout.take().expect("the child's output");
    let mut stderr = child.stderr.take().expect("the child's diagnostics");
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
    let deadline = Instant::now() + limit;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            break Some(status.code());
        }
        if Instant::now() >= deadline {
            drop(child.kill());
            drop(child.wait());
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    Ok(Ran {
        stdout: printed.join().expect("the child's output read"),
        stderr: reported.join().expect("the child's diagnostics read"),
        status,
    })
}

/// The workspace and local store directories of the NaN toolchain's runs beside the
/// bootstrap's.
fn fronts(bootstrap: &Path, store: &Path) -> (PathBuf, PathBuf) {
    let mut front_dir = bootstrap.as_os_str().to_owned();
    front_dir.push("-front");
    let mut front_store = store.as_os_str().to_owned();
    front_store.push("-front");
    (PathBuf::from(front_dir), PathBuf::from(front_store))
}

/// The user directory of the NaN toolchain's runs.
fn front_home() -> PathBuf {
    base().join("home")
}

/// Makes the NaN toolchain's workspace and local store copies of the bootstrap's.
fn resync(bootstrap: &Path, store: &Path) -> Result<(), String> {
    let fault = |error: std::io::Error| error.to_string();
    let (front_dir, front_store) = fronts(bootstrap, store);
    copy_tree(bootstrap, &front_dir, true).map_err(fault)?;
    match store.exists() {
        true => copy_tree(store, &front_store, true).map_err(fault),
        false => {
            drop(fs::remove_dir_all(&front_store));
            Ok(())
        }
    }
}

/// The files under `dir`, by path, with their contents, but those of `.nan/images`.
fn tree(dir: &Path, prefix: &str, files: &mut Vec<(String, Vec<u8>)>) -> std::io::Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = format!("{prefix}{}", entry.file_name().to_string_lossy());
        if path == ".nan/images" {
            continue;
        }
        match entry.file_type()?.is_dir() {
            true => tree(&entry.path(), &format!("{path}/"), files)?,
            false => files.push((path, fs::read(entry.path())?)),
        }
    }
    Ok(())
}

/// The first path at which two trees differ, if any.
fn tree_difference(left: &Path, right: &Path) -> Result<Option<String>, String> {
    let fault = |error: std::io::Error| error.to_string();
    let (mut ours, mut theirs) = (Vec::new(), Vec::new());
    tree(left, "", &mut ours).map_err(fault)?;
    tree(right, "", &mut theirs).map_err(fault)?;
    for index in 0..ours.len().max(theirs.len()) {
        match (ours.get(index), theirs.get(index)) {
            (Some(one), Some(other)) if one == other => {}
            (Some((path, _)), _) | (None, Some((path, _))) => return Ok(Some(path.clone())),
            (None, None) => {}
        }
    }
    Ok(None)
}

/// Runs `nan <args>` with the bootstrap in its workspace and with the NaN toolchain in a copy of
/// it and of its local store, then compares what both print, the statuses they exit with and the
/// files they leave in the workspace and the local store. Gives what they print and the status.
fn compare(
    toolchain: &Toolchain,
    bootstrap: &Path,
    store: &Path,
    args: &[&str],
) -> Result<(String, Option<i32>), String> {
    resync(bootstrap, store)?;
    let (front_dir, front_store) = fronts(bootstrap, store);
    let (expected, status) = nan(bootstrap, args)?;
    let home = front_home();
    let given = Given {
        home: &home,
        store: &front_store,
        asked: &[],
    };
    let (printed, code) = front(toolchain, &front_dir, &given, args)?;
    if printed != expected.as_bytes() || code != status {
        return Err(format!(
            "nan {} exits with {status:?} and prints\n{expected}the NaN toolchain exits with {code:?} and prints\n{}",
            args.join(" "),
            String::from_utf8_lossy(&printed)
        ));
    }
    for (ours, theirs) in [(bootstrap, front_dir.as_path()), (store, &front_store)] {
        if let Some(path) = tree_difference(ours, theirs)? {
            return Err(format!(
                "after nan {}, {path} differs in {}",
                args.join(" "),
                theirs.display()
            ));
        }
    }
    Ok((expected, status))
}

/// Compares the canonical NIR stream of each package of the workspace, or of the one `package`
/// names, with the NaN toolchain's, run on its copy of the workspace.
fn compare_nir(
    toolchain: &Toolchain,
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
    resync(bootstrap, store)?;
    let (front_dir, front_store) = fronts(bootstrap, store);
    let home = front_home();
    let asked = [("NAN_SELF_CHECK_NIR", package.unwrap_or_default())];
    let given = Given {
        home: &home,
        store: &front_store,
        asked: &asked,
    };
    let (printed, code) = front(toolchain, &front_dir, &given, &[])?;
    if printed != expected || code != Some(0) {
        return Err(format!(
            "the NaN toolchain exits with {code:?} and gives NIR differing from the bootstrap's at byte {}",
            printed
                .iter()
                .zip(&expected)
                .position(|(given, wanted)| given != wanted)
                .unwrap_or(printed.len().min(expected.len()))
        ));
    }
    Ok(())
}

/// The functions a lowering dump holds: each symbol name with its text.
fn lowered(dump: &[u8]) -> Result<Vec<(String, String)>, String> {
    let mut functions = Vec::new();
    let mut at = 0;
    while at < dump.len() {
        let end = at
            + dump[at..]
                .iter()
                .position(|byte| *byte == b'\n')
                .ok_or("a lowering dump's line ends")?;
        let line = String::from_utf8_lossy(&dump[at..end]).into_owned();
        let (name, length) = line.rsplit_once(' ').ok_or("a name and a length")?;
        let length: usize = length.parse().map_err(|_| "a text's length")?;
        let text = dump
            .get(end + 1..end + 1 + length)
            .ok_or("a function's text")?;
        functions.push((name.to_owned(), String::from_utf8_lossy(text).into_owned()));
        at = end + 1 + length;
    }
    Ok(functions)
}

/// Compares the text of each function the final build of `program` lowers with the bootstrap's.
fn compare_lowered(
    toolchain: &Toolchain,
    bootstrap: &Path,
    store: &Path,
    program: &str,
) -> Result<(), String> {
    let expected = nan::lowered_functions(bootstrap, program).map_err(|error| error.to_string())?;
    resync(bootstrap, store)?;
    let (front_dir, front_store) = fronts(bootstrap, store);
    let home = front_home();
    let asked = [("NAN_SELF_CHECK_LOWERED", program)];
    let given = Given {
        home: &home,
        store: &front_store,
        asked: &asked,
    };
    let (printed, code) = front(toolchain, &front_dir, &given, &[])?;
    if code != Some(0) {
        return Err(format!(
            "the NaN toolchain exits with {code:?} lowering {program}"
        ));
    }
    let given = lowered(&printed)?;
    for index in 0..expected.len().max(given.len()) {
        match (expected.get(index), given.get(index)) {
            (Some(wanted), Some(found)) if wanted == found => {}
            (wanted, found) => {
                let text = |function: Option<&(String, String)>| {
                    function.map_or_else(
                        || "nothing".to_owned(),
                        |(name, text)| format!("{name}\n{text}"),
                    )
                };
                return Err(format!(
                    "function {index} of {program}'s lowering is, in the bootstrap's,\n{}\nin the NaN toolchain's,\n{}",
                    text(wanted),
                    text(found)
                ));
            }
        }
    }
    Ok(())
}

/// Runs the `x86_64-host` image of `program` that each toolchain built in its workspace and
/// compares what both write to their standard streams and the statuses they exit with.
fn compare_program(bootstrap: &Path, store: &Path, program: &str) -> Result<(), String> {
    let (front_dir, _) = fronts(bootstrap, store);
    let mut runs = Vec::new();
    for dir in [bootstrap, front_dir.as_path()] {
        let mut command = Command::new(dir.join(".nan/images/x86_64-host").join(program));
        command
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        runs.push(supervised(command, RUN_LIMIT)?);
    }
    if runs[0] != runs[1] {
        let written = |ran: &Ran| {
            format!(
                "{}{}exiting with {:?}",
                String::from_utf8_lossy(&ran.stdout),
                String::from_utf8_lossy(&ran.stderr),
                ran.status
            )
        };
        return Err(format!(
            "{program} built by the bootstrap writes\n{}\nbuilt by the NaN toolchain\n{}",
            written(&runs[0]),
            written(&runs[1])
        ));
    }
    Ok(())
}

/// Compares, for an example, every command the conformance harness runs: checking, NIR, tests,
/// the final build and lowering of each program with what each `x86_64-host` one writes,
/// `nan verify`, the commands of its section's `text` blocks and `nan fmt` of its files without
/// their marker comments.
fn compare_example(
    toolchain: &Toolchain,
    example: &Example,
    dir: &Path,
    store: &Path,
) -> Result<(), String> {
    example
        .write(dir, false)
        .map_err(|error| error.to_string())?;
    example.prepare(dir)?;
    let (_, status) = compare(toolchain, dir, store, &["check"])?;
    compare_nir(toolchain, dir, store, None)?;
    if status == Some(0) {
        compare(toolchain, dir, store, &["test"])?;
        for program in programs(example) {
            let (_, status) = compare(toolchain, dir, store, &["build", &program.0])?;
            if status == Some(0) {
                compare_lowered(toolchain, dir, store, &program.0)?;
                if program.1 == "x86_64-host" {
                    compare_program(dir, store, &program.0)?;
                }
            }
        }
    }
    compare(toolchain, dir, store, &["verify"])?;
    for (command, _) in &example.commands {
        let args: Vec<&str> = command.split(' ').skip(1).collect();
        if args.first() != Some(&"run") {
            compare(toolchain, dir, store, &args)?;
        }
    }
    example
        .write(dir, true)
        .map_err(|error| error.to_string())?;
    compare(toolchain, dir, store, &["fmt"])?;
    Ok(())
}

/// The programs of an example: each package declaring an entry and a concrete target, by name and
/// target.
fn programs(example: &Example) -> Vec<(String, String)> {
    example
        .files
        .iter()
        .filter(|(path, _)| path.ends_with("/package.nan"))
        .filter_map(|(path, lines)| {
            let entry = lines.iter().any(|line| unmark(line).trim() == "entry main");
            let target = lines.iter().find_map(|line| {
                let target = line.trim().strip_prefix("targets ")?;
                ["x86_64-host", "x86_64-uefi", "x86_64-bare"]
                    .into_iter()
                    .find(|known| target.split(", ").any(|one| one == *known))
            })?;
            let package = path.split_once('/')?.0.to_owned();
            entry.then_some((package, target.to_owned()))
        })
        .collect()
}

fn chapter(path: &Utf8Path, text: String) -> datatest_stable::Result<()> {
    let chapter = path.file_stem().expect("a chapter file");
    let store = fresh_store(&format!("self-check-{chapter}"))?;
    let toolchain = Toolchain::new(image()?);
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
        match compare_example(&toolchain, &example, &dir, &store) {
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
    let toolchain = Toolchain::new(image()?);
    let dir = base().join("std").join(package);
    copy_tree(&repository().join("std"), &dir, false)?;
    compare(&toolchain, &dir, &store, &["check", package])?;
    compare_nir(&toolchain, &dir, &store, Some(package))?;
    compare(&toolchain, &dir, &store, &["test", package])?;
    fs::remove_dir_all(&dir)?;
    fs::remove_dir_all(fronts(&dir, &store).0)?;
    Ok(())
}

fn program(path: &Utf8Path, _: String) -> datatest_stable::Result<()> {
    let source = path.parent().expect("a program directory");
    let name = source.file_name().expect("a program name");
    let store = fresh_store(&format!("self-check-program-{name}"))?;
    let toolchain = Toolchain::new(image()?);
    let dir = base().join("programs").join(name);
    copy_tree(source.as_std_path(), &dir.join(name), true)?;
    fs::remove_file(dir.join(name).join("expected"))?;
    compare(&toolchain, &dir, &store, &["build", name])?;
    compare_lowered(&toolchain, &dir, &store, name)?;
    compare_program(&dir, &store, name)?;
    fs::remove_dir_all(&dir)?;
    fs::remove_dir_all(fronts(&dir, &store).0)?;
    Ok(())
}

/// Builds the toolchain workspace copied into `dir` with the NaN toolchain `builder`, with a user
/// directory and local store of its own, and gives the toolchain it writes.
fn self_build(builder: &Toolchain, dir: &Path) -> Result<Toolchain, String> {
    driven_copy(dir)?;
    let home = dir.with_extension("home");
    let store = dir.with_extension("store");
    drop(fs::remove_dir_all(&home));
    let given = Given {
        home: &home,
        store: &store,
        asked: &[],
    };
    let (printed, code) = front(builder, dir, &given, &["build", "nan"])?;
    if code != Some(0) {
        return Err(format!(
            "{} does not build the toolchain:\n{}",
            builder.image.display(),
            String::from_utf8_lossy(&printed)
        ));
    }
    Ok(Toolchain::over_itself(
        dir.join(".nan/images/x86_64-host/nan"),
    ))
}

/// Checks the toolchain workspace with both, which must agree and report no diagnostic, and
/// verifies it with both, in a copy outside every git working tree. Then the toolchain the
/// bootstrap built builds the toolchain, the toolchain so built builds it again into the same
/// bytes, and the bootstrap's stage comparison accepts the first self-built toolchain.
fn toolchain(_: &Utf8Path, _: String) -> datatest_stable::Result<()> {
    let store = fresh_store("self-check-toolchain")?;
    let first = Toolchain::over_itself(image()?);
    let outside = std::env::temp_dir().join(format!("nan-self-check-{}", std::process::id()));
    let dir = outside.join("toolchain");
    copy_tree(&repository().join("toolchain"), &dir, false)?;
    let (output, _) = compare(&first, &dir, &store, &["check"])?;
    if output.lines().any(|line| !line.starts_with("ok check ")) {
        return Err(format!("the toolchain workspace checks with diagnostics:\n{output}").into());
    }
    compare_nir(&first, &dir, &store, None)?;
    let (output, _) = compare(&first, &dir, &store, &["verify"])?;
    if !output.starts_with("ok ") {
        return Err(format!("nan verify reports\n{output}").into());
    }
    let second = self_build(&first, &outside.join("second"))?;
    let third = self_build(&second, &outside.join("third"))?;
    if fs::read(&second.image)? != fs::read(&third.image)? {
        return Err("the toolchain it built builds a toolchain of other bytes".into());
    }
    let compared = outside.join("compared");
    driven_copy(&compared)?;
    let second_path = second.image.to_str().expect("a UTF-8 path");
    let (output, status) = nan(&compared, &["build", "nan", "--compare-with", second_path])?;
    if status != Some(0) {
        return Err(format!("the stage comparison reports\n{output}").into());
    }
    fs::remove_dir_all(outside)?;
    Ok(())
}
