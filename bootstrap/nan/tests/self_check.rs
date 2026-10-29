//! The self-check harness: builds the NaN toolchain of `toolchain/` once per state of its files
//! and of this harness, with a root file of its own, then runs it beside the bootstrap and
//! compares what both print, their exit statuses and the files they write: for every `nan`
//! example of each specification chapter, the commands the conformance harness runs and those of
//! its section's `text` blocks, with the canonical NIR, the text of each function a final build
//! lowers and what each host program of this machine's architecture writes; for every std
//! package, `nan check` with its NIR and `nan test`; for every program of `tests/programs`, its
//! final build and lowering and what it writes, a machine program's under its emulator; and for
//! the toolchain workspace, which must check without a diagnostic and pass `nan verify` with
//! both. The toolchain test then checks the self-built toolchain by diverse
//! double-compiling: the toolchain the bootstrap built builds the toolchain, which builds it again
//! into the same bytes, and the bootstrap's stage comparison accepts it. The NaN toolchain runs
//! beside the bootstrap on a copy of the workspace and local store the bootstrap runs in, on one
//! processor under an address-space cap, with a user directory of its own.

use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use datatest_stable::Utf8Path;

mod common;
mod machine;

use common::{Example, examples, nan, unmark};

datatest_stable::harness! {
    { test = chapter, root = "../../docs/spec", pattern = r"^\d\d-[a-z-]+\.md$" },
    { test = std_package, root = "../../std", pattern = r"^[a-z_]+/package\.nan$" },
    { test = program, root = "tests/programs", pattern = r"^[a-z0-9_]+/expected$" },
    { test = toolchain, root = "../../toolchain", pattern = r"^nan/package\.nan$" },
    { test = internal_error, root = "../../toolchain", pattern = r"^nan/nan\.nan$" },
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
        return try await command::entry(take world, home: given.home)
    }
    let Ok(directory) = attempt platform::current_dir(alloc) else {
        return 101
    }
    let Ok(current) = attempt text::String.try_from_utf8(directory.view(), alloc) else {
        return 101
    }
    let root = workspace::root_of(current.as_str(), alloc)
    let grammar = syntax::Grammar.load(alloc)
    let read = mem::Arena[text::String].empty(alloc)
    let Ok(loaded) = attempt workspace::Workspace.load(root.as_str(), read, given: given.home, grammar, alloc) else {
        return 101
    }
    let session = check::Session.of(mem::Shared.of(take loaded, alloc: alloc), std: workspace::CarriedStd.unloaded(parsing: mem::Shared.of(workspace::Parsing.empty(served: false, alloc), alloc: alloc), alloc), grammar: mem::Shared.of(take grammar, alloc: alloc), home: cache::copied_override(given.home, alloc), alloc)
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
const RUN_SPACE: u64 = 6_000_000;
const SELF_SPACE: u64 = 8_000_000;

/// An address space, in KiB, in which the NaN toolchain starts but cannot check its workspace.
const STARVED_SPACE: u64 = 800_000;

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

/// One processor of the harness's affinity set, the next one for each run this process starts,
/// from one its process id picks, so that runs beside each other do not share a processor. The
/// runtime of an image sets up an executor thread and an io_uring instance per processor of its
/// affinity set, and the kernel charges the instances' memory to the user's locked-memory limit,
/// which concurrent toolchain runs with every processor would exhaust.
fn processor() -> String {
    static STARTED: AtomicUsize = AtomicUsize::new(0);
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
    let run = std::process::id() as usize + STARTED.fetch_add(1, Ordering::Relaxed);
    allowed[run % allowed.len()].to_string()
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

/// Starts a run of a NaN toolchain in `dir`, on one processor, under its bounds, killed after its
/// time limit.
fn front(toolchain: &Toolchain, dir: &Path, given: &Given, args: &[&str]) -> Result<Front, String> {
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
    Ok(Front {
        run: Supervised::start(command, toolchain.limit)?,
        limit: toolchain.limit,
        command: args.join(" "),
    })
}

/// A started run of a NaN toolchain, with its time limit and command line.
struct Front {
    run: Supervised,
    limit: Duration,
    command: String,
}

impl Front {
    /// What the run prints and the status it exits with.
    fn output(self) -> Result<(Vec<u8>, Option<i32>), String> {
        let ran = self.run.finish()?;
        let Some(status) = ran.status else {
            return Err(format!(
                "the NaN toolchain runs over {:?}: nan {}",
                self.limit, self.command
            ));
        };
        if !ran.stderr.is_empty() {
            return Err(format!(
                "the NaN toolchain exits with {status:?} on nan {} and reports\n{}",
                self.command,
                String::from_utf8_lossy(&ran.stderr)
            ));
        }
        Ok((ran.stdout, status))
    }
}

/// What a supervised command writes to its standard output and error, and its exit status,
/// none when it was killed.
#[derive(Debug, PartialEq)]
struct Ran {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    status: Option<Option<i32>>,
}

/// A started command, its standard output and error read as it runs, killed once its deadline
/// passes or when dropped unfinished.
struct Supervised {
    child: Child,
    printed: Option<JoinHandle<Vec<u8>>>,
    reported: Option<JoinHandle<Vec<u8>>>,
    deadline: Instant,
}

impl Supervised {
    /// Starts a command that may run for `limit`.
    fn start(mut command: Command, limit: Duration) -> Result<Supervised, String> {
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
        Ok(Supervised {
            child,
            printed: Some(printed),
            reported: Some(reported),
            deadline: Instant::now() + limit,
        })
    }

    /// Waits for the command to end, or kills it once its deadline passes.
    fn finish(mut self) -> Result<Ran, String> {
        let status = loop {
            if let Some(status) = self.child.try_wait().map_err(|error| error.to_string())? {
                break Some(status.code());
            }
            if Instant::now() >= self.deadline {
                drop(self.child.kill());
                drop(self.child.wait());
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let read = |handle: Option<JoinHandle<Vec<u8>>>| {
            handle
                .expect("a stream read once")
                .join()
                .expect("the child's stream read")
        };
        Ok(Ran {
            stdout: read(self.printed.take()),
            stderr: read(self.reported.take()),
            status,
        })
    }
}

impl Drop for Supervised {
    fn drop(&mut self) {
        drop(self.child.kill());
        drop(self.child.wait());
    }
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
    let (front_dir, front_store) = fronts(bootstrap, store);
    copy_state((bootstrap, store), (&front_dir, &front_store))
}

/// Makes a workspace and local store copies of others.
fn copy_state(from: (&Path, &Path), to: (&Path, &Path)) -> Result<(), String> {
    let fault = |error: std::io::Error| error.to_string();
    copy_tree(from.0, to.0, true).map_err(fault)?;
    match from.1.exists() {
        true => copy_tree(from.1, to.1, true).map_err(fault),
        false => {
            drop(fs::remove_dir_all(to.1));
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

/// Runs `nan <args>` with the bootstrap in its workspace and, at the same time, with the NaN
/// toolchain in a copy of it and of its local store, then compares what both print, the statuses
/// they exit with and the files they leave in the workspace and the local store. Gives what they
/// print and the status.
fn compare(
    toolchain: &Toolchain,
    bootstrap: &Path,
    store: &Path,
    args: &[&str],
) -> Result<(String, Option<i32>), String> {
    resync(bootstrap, store)?;
    let (front_dir, front_store) = fronts(bootstrap, store);
    let home = front_home();
    let given = Given {
        home: &home,
        store: &front_store,
        asked: &[],
    };
    let run = front(toolchain, &front_dir, &given, args)?;
    let (expected, status) = nan(bootstrap, args)?;
    let (printed, code) = run.output()?;
    agree(args, (expected.as_bytes(), status), (&printed, code))?;
    same_files(args, (bootstrap, store), (&front_dir, &front_store))?;
    Ok((expected, status))
}

/// Compares what the bootstrap and the NaN toolchain print on `nan <args>` and the statuses they
/// exit with.
fn agree(
    args: &[&str],
    (expected, status): (&[u8], Option<i32>),
    (printed, code): (&[u8], Option<i32>),
) -> Result<(), String> {
    if printed == expected && code == status {
        return Ok(());
    }
    Err(format!(
        "nan {} exits with {status:?} and prints\n{}the NaN toolchain exits with {code:?} and prints\n{}",
        args.join(" "),
        String::from_utf8_lossy(expected),
        String::from_utf8_lossy(printed)
    ))
}

/// Compares the workspace and local store the bootstrap leaves after `nan <args>` with those the
/// NaN toolchain leaves.
fn same_files(args: &[&str], ours: (&Path, &Path), theirs: (&Path, &Path)) -> Result<(), String> {
    for (ours, theirs) in [(ours.0, theirs.0), (ours.1, theirs.1)] {
        if let Some(path) = tree_difference(ours, theirs)? {
            return Err(format!(
                "after nan {}, {path} differs in {}",
                args.join(" "),
                theirs.display()
            ));
        }
    }
    Ok(())
}

/// Compares the canonical NIR stream of each package of the workspace, or of the one `package`
/// names, with the NaN toolchain's, run on its copy of the workspace at the same time.
fn compare_nir(
    toolchain: &Toolchain,
    bootstrap: &Path,
    store: &Path,
    package: Option<&str>,
) -> Result<(), String> {
    resync(bootstrap, store)?;
    let (front_dir, front_store) = fronts(bootstrap, store);
    let home = front_home();
    let asked = [("NAN_SELF_CHECK_NIR", package.unwrap_or_default())];
    let given = Given {
        home: &home,
        store: &front_store,
        asked: &asked,
    };
    let run = front(toolchain, &front_dir, &given, &[])?;
    let streams = nan::nir_streams(bootstrap).map_err(|error| error.to_string())?;
    let mut expected = Vec::new();
    for (name, stream) in &streams {
        if package.is_some_and(|package| package != name) {
            continue;
        }
        expected.extend(format!("{name} {}\n", stream.len()).bytes());
        expected.extend(stream);
    }
    let (printed, code) = run.output()?;
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

/// The support routines the NaN toolchain lowers its own way on `x86_64-host`, whose programs
/// the harness compares by what they write instead.
const OWN_ROUTINES: [&str; 17] = [
    "_NScap_table_alloc",
    "_NScap_table_alloc_in",
    "_NScap_table_alloc_large",
    "_NScap_table_chunk_return",
    "_NScap_table_free",
    "_NScap_table_free_unpooled",
    "_NScap_table_lift",
    "_NScap_table_pool_add",
    "_NScap_table_pool_draw",
    "_NSstack_switch_init",
    "_NSstack_switch_lock",
    "_NSstack_switch_lock_wait",
    "_NSstack_switch_thread_main",
    "_NSstack_switch_unlock",
    "_NStrap_overflow",
    "_NStrap_preempt",
    "_NStrap_raise",
];

/// Compares the text of each function the final build of `program` lowers with the bootstrap's,
/// for an `x86_64-host` program those `comparable` keeps, as it gives them.
fn compare_lowered(
    toolchain: &Toolchain,
    bootstrap: &Path,
    store: &Path,
    program: &str,
    host: bool,
) -> Result<(), String> {
    resync(bootstrap, store)?;
    let (front_dir, front_store) = fronts(bootstrap, store);
    let home = front_home();
    let asked = [("NAN_SELF_CHECK_LOWERED", program)];
    let given = Given {
        home: &home,
        store: &front_store,
        asked: &asked,
    };
    let run = front(toolchain, &front_dir, &given, &[])?;
    let mut expected =
        nan::lowered_functions(bootstrap, program).map_err(|error| error.to_string())?;
    let (printed, code) = run.output()?;
    if code != Some(0) {
        return Err(format!(
            "the NaN toolchain exits with {code:?} lowering {program}"
        ));
    }
    let mut given = lowered(&printed)?;
    if host {
        expected = comparable(expected);
        given = comparable(given);
    }
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

/// The prefix of the symbols of std's `hw` package, whose synchronisation operations read the
/// line tables the NaN toolchain lays out its own way on `x86_64-host`, so that the harness
/// compares the programs calling them by what they write instead.
const HW_FUNCTIONS: &str = "_N2hw_";

/// The functions of a lowering but `OWN_ROUTINES` and `HW_FUNCTIONS`, in the order of their
/// names: which routines and C library functions those call first, and so the numbers of the
/// names lowering declares after, differ. Each external name a text holds is numbered by its
/// first mention instead.
fn comparable(functions: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut kept: Vec<(String, String)> = functions
        .into_iter()
        .filter(|(name, _)| {
            !OWN_ROUTINES.contains(&name.as_str()) && !name.starts_with(HW_FUNCTIONS)
        })
        .collect();
    kept.sort_by(|left, right| left.0.cmp(&right.0));
    let mut numbers = std::collections::HashMap::new();
    kept.into_iter()
        .map(|(name, text)| {
            let text = renumbered(&text, &mut numbers);
            (name, text)
        })
        .collect()
}

/// A function's text with each external name `uN:M` written `uN:` and the number of its first
/// mention in `numbers`.
fn renumbered(text: &str, numbers: &mut std::collections::HashMap<String, usize>) -> String {
    let bytes = text.as_bytes();
    let digits_from = |from: usize| {
        from + bytes[from.min(bytes.len())..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count()
    };
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    let mut at = 0;
    while at < bytes.len() {
        let starts = bytes[at] == b'u'
            && (at == 0 || !(bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_'));
        let colon = digits_from(at + 1);
        let end = digits_from(colon + 1);
        if starts && colon > at + 1 && bytes.get(colon) == Some(&b':') && end > colon + 1 {
            let next = numbers.len();
            let number = *numbers.entry(text[at..end].to_owned()).or_insert(next);
            out.push_str(&text[copied..=colon]);
            out.push_str(&number.to_string());
            copied = end;
            at = end;
        } else {
            at += 1;
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// Runs the images of `program` for `target` that each toolchain built in its workspace and
/// compares what both write and the statuses they end with: a host program's standard streams
/// when this machine has its architecture, a machine program's serial port under its emulator.
fn compare_program(
    toolchain: &Toolchain,
    bootstrap: &Path,
    store: &Path,
    program: &str,
    target: &str,
) -> Result<(), String> {
    match target.strip_suffix("-host") {
        Some(arch) if arch != std::env::consts::ARCH => Ok(()),
        Some(_) => compare_processes(bootstrap, store, program, target),
        None => compare_boots(toolchain, bootstrap, store, program),
    }
}

/// Runs the host image of `program` for `target` that each toolchain built in its workspace and
/// compares what both write to their standard streams and the statuses they exit with.
fn compare_processes(
    bootstrap: &Path,
    store: &Path,
    program: &str,
    target: &str,
) -> Result<(), String> {
    let (front_dir, _) = fronts(bootstrap, store);
    let mut runs = Vec::new();
    for dir in [bootstrap, front_dir.as_path()] {
        let mut command = Command::new(dir.join(".nan/images").join(target).join(program));
        command
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        runs.push(Supervised::start(command, RUN_LIMIT)?.finish()?);
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

/// Boots the machine image of `program` that each toolchain builds in its workspace with
/// `nan run` and compares what both write to the serial port and the statuses their runs end
/// with.
fn compare_boots(
    toolchain: &Toolchain,
    bootstrap: &Path,
    store: &Path,
    program: &str,
) -> Result<(), String> {
    let run = nan::run(&["run".to_owned(), program.to_owned()], bootstrap);
    let launch = run
        .launch
        .ok_or_else(|| format!("nan run {program} reports\n{}", run.output))?;
    let booted = machine::boot(&launch, bootstrap)?;
    let (front_dir, front_store) = fronts(bootstrap, store);
    let mut command = Command::new(&toolchain.image);
    command
        .args(["run", program])
        .current_dir(&front_dir)
        .env("NAN_SELF_CHECK_HOME", front_home())
        .env("NAN_SELF_CHECK_STORE", &front_store)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let ran = Supervised::start(command, RUN_LIMIT)?.finish()?;
    let written = String::from_utf8_lossy(&ran.stdout);
    if ran.status != Some(Some(booted.status)) || written != booted.written {
        return Err(format!(
            "{program} built by the bootstrap writes\n{}exiting with {:?}\nbuilt by the NaN toolchain\n{written}exiting with {:?}{}",
            booted.written,
            Some(Some(booted.status)),
            ran.status,
            String::from_utf8_lossy(&ran.stderr)
        ));
    }
    Ok(())
}

/// Compares, for an example, every command the conformance harness runs: checking, NIR, tests,
/// the final build and lowering of each program with what each host one of this machine's
/// architecture writes, `nan verify`, the commands of its section's `text` blocks and `nan fmt`
/// of its files without their marker comments.
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
                let host = program.1.strip_suffix("-host");
                compare_lowered(toolchain, dir, store, &program.0, host.is_some())?;
                if host == Some(std::env::consts::ARCH) {
                    compare_processes(dir, store, &program.0, &program.1)?;
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

/// The concrete targets.
const TARGETS: [&str; 6] = [
    "x86_64-host",
    "x86_64-uefi",
    "x86_64-bare",
    "aarch64-host",
    "aarch64-uefi",
    "aarch64-bare",
];

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
                TARGETS
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
    let manifest = fs::read_to_string(dir.join(name).join("package.nan"))?;
    let target = manifest
        .lines()
        .find_map(|line| line.trim().strip_prefix("targets "))
        .expect("a program's targets directive")
        .to_owned();
    compare_lowered(&toolchain, &dir, &store, name, target.ends_with("-host"))?;
    compare_program(&toolchain, &dir, &store, name, &target)?;
    fs::remove_dir_all(&dir)?;
    fs::remove_dir_all(fronts(&dir, &store).0)?;
    Ok(())
}

/// Starts building the toolchain workspace copied into `dir` with the NaN toolchain `builder`,
/// with a user directory and local store of its own.
fn self_build(builder: &Toolchain, dir: &Path) -> Result<SelfBuild, String> {
    driven_copy(dir)?;
    let home = dir.with_extension("home");
    let store = dir.with_extension("store");
    drop(fs::remove_dir_all(&home));
    let given = Given {
        home: &home,
        store: &store,
        asked: &[],
    };
    Ok(SelfBuild {
        run: front(builder, dir, &given, &["build", "nan"])?,
        builder: builder.image.clone(),
        image: dir.join(".nan/images/x86_64-host/nan"),
    })
}

/// A started build of the toolchain workspace by a NaN toolchain, and the image it writes.
struct SelfBuild {
    run: Front,
    builder: PathBuf,
    image: PathBuf,
}

impl SelfBuild {
    /// The toolchain the build writes.
    fn built(self) -> Result<Toolchain, String> {
        let (printed, code) = self.run.output()?;
        if code != Some(0) {
            return Err(format!(
                "{} does not build the toolchain:\n{}",
                self.builder.display(),
                String::from_utf8_lossy(&printed)
            ));
        }
        Ok(Toolchain::over_itself(self.image))
    }
}

/// The commands of the toolchain test, in order.
const STEPS: [&[&str]; 2] = [&["check"], &["verify"]];

/// What the bootstrap gave on a command of the toolchain test: what it printed, the status it
/// exited with and copies of the workspace and local store it left.
struct Taken {
    expected: String,
    status: Option<i32>,
    left: (PathBuf, PathBuf),
}

/// The copies, named after `kind`, of the workspace and local store of a command of the toolchain
/// test.
fn step_copies(outside: &Path, kind: &str, step: usize) -> (PathBuf, PathBuf) {
    let dir = outside.join(format!("{kind}-{step}"));
    let store = dir.with_extension("store");
    (dir, store)
}

/// Checks the toolchain workspace with both, which must agree and report no diagnostic, compares
/// their NIR of the checked workspace and verifies it with both, in a copy outside every git
/// working tree. The bootstrap runs `nan check` then `nan verify` from the start, and the
/// toolchain it built runs each on a copy of the workspace and local store the bootstrap made
/// before running it. Meanwhile the toolchain the bootstrap built builds the toolchain, the
/// toolchain so built builds it again into the same bytes, and the bootstrap's stage comparison
/// accepts the first self-built toolchain.
fn toolchain(_: &Utf8Path, _: String) -> datatest_stable::Result<()> {
    let store = fresh_store("self-check-toolchain")?;
    let outside = std::env::temp_dir().join(format!("nan-self-check-{}", std::process::id()));
    let dir = outside.join("toolchain");
    copy_tree(&repository().join("toolchain"), &dir, false)?;
    let (copied_to, copied) = mpsc::channel();
    let (taken_to, taken) = mpsc::channel();
    let (bootstrapped, fronted) = std::thread::scope(|scope| {
        let bootstrapped =
            scope.spawn(|| bootstrap_steps((&dir, &store), &outside, copied_to, taken_to));
        let fronted = front_side(&outside, copied, taken);
        (
            bootstrapped.join().expect("the bootstrap's commands end"),
            fronted,
        )
    });
    bootstrapped?;
    fronted?;
    fs::remove_dir_all(outside)?;
    Ok(())
}

/// Runs each command of the toolchain test with the bootstrap in the workspace and local store
/// `state`, which it copies for the NaN toolchain before the command and as it left them after.
/// Stops once nothing waits for its commands.
fn bootstrap_steps(
    state: (&Path, &Path),
    outside: &Path,
    copied: Sender<()>,
    taken: Sender<Taken>,
) -> Result<(), String> {
    for (step, args) in STEPS.iter().enumerate() {
        let (front_dir, front_store) = step_copies(outside, "front", step);
        copy_state(state, (&front_dir, &front_store))?;
        if copied.send(()).is_err() {
            return Ok(());
        }
        let (expected, status) = nan(state.0, args)?;
        let (left_dir, left_store) = step_copies(outside, "left", step);
        copy_state(state, (&left_dir, &left_store))?;
        let given = Taken {
            expected,
            status,
            left: (left_dir, left_store),
        };
        if taken.send(given).is_err() {
            return Ok(());
        }
    }
    Ok(())
}

/// Builds the toolchain image, then runs the double-compiling check beside the NaN toolchain's
/// side of the toolchain test's commands.
fn front_side(outside: &Path, copied: Receiver<()>, taken: Receiver<Taken>) -> Result<(), String> {
    let first = Toolchain::over_itself(image()?);
    let second = self_build(&first, &outside.join("second"))?;
    std::thread::scope(|scope| {
        let staged = scope.spawn(|| double_compiled(second, outside));
        let compared = front_steps(&first, outside, copied, taken);
        let staged = staged.join().expect("the double-compiling check ends");
        compared.and(staged)
    })
}

/// Runs each command of the toolchain test with the NaN toolchain, once the bootstrap has copied
/// the workspace and local store for it, and compares it with the bootstrap's; after `nan check`,
/// compares their NIR of the workspace the bootstrap's check left.
fn front_steps(
    first: &Toolchain,
    outside: &Path,
    copied: Receiver<()>,
    taken: Receiver<Taken>,
) -> Result<(), String> {
    let home = front_home();
    let ended = |_| "the bootstrap's side of the toolchain test ends early".to_owned();
    for (step, args) in STEPS.iter().enumerate() {
        copied.recv().map_err(ended)?;
        let (front_dir, front_store) = step_copies(outside, "front", step);
        let given = Given {
            home: &home,
            store: &front_store,
            asked: &[],
        };
        let run = front(first, &front_dir, &given, args)?;
        let bootstrap = taken.recv().map_err(ended)?;
        let (printed, code) = run.output()?;
        let (left_dir, left_store) = &bootstrap.left;
        agree(
            args,
            (bootstrap.expected.as_bytes(), bootstrap.status),
            (&printed, code),
        )?;
        same_files(args, (left_dir, left_store), (&front_dir, &front_store))?;
        let output = &bootstrap.expected;
        let accepted = match *args {
            ["check"] => output.lines().all(|line| line.starts_with("ok check ")),
            _ => output.starts_with("ok "),
        };
        if !accepted {
            return Err(format!("nan {} reports\n{output}", args.join(" ")));
        }
        if *args == ["check"] {
            compare_nir(first, left_dir, left_store, None)?;
        }
    }
    Ok(())
}

/// Waits for the first self-built toolchain, then builds the toolchain again with it, under
/// `outside`, into the same bytes, while the bootstrap's stage comparison against it must report
/// no error.
fn double_compiled(second: SelfBuild, outside: &Path) -> Result<(), String> {
    let fault = |error: std::io::Error| error.to_string();
    let second = second.built()?;
    let third = self_build(&second, &outside.join("third"))?;
    let compared = outside.join("compared");
    driven_copy(&compared)?;
    let second_path = second.image.to_str().expect("a UTF-8 path");
    let (output, status) = nan(&compared, &["build", "nan", "--compare-with", second_path])?;
    if status != Some(0) {
        return Err(format!("the stage comparison reports\n{output}"));
    }
    let third = third.built()?;
    if fs::read(&second.image).map_err(fault)? != fs::read(&third.image).map_err(fault)? {
        return Err("the toolchain it built builds a toolchain of other bytes".to_owned());
    }
    Ok(())
}

/// A trap of the NaN toolchain's own code is an internal error: checking its workspace with an
/// empty user directory in too little address space exhausts the memory of the domain the command
/// runs in, which it reports
/// as `internal.resource-exhausted` with a reproducer holding the command line and the files the
/// command read, writing nothing to standard error.
fn internal_error(_: &Utf8Path, _: String) -> datatest_stable::Result<()> {
    let store = fresh_store("self-check-internal-error")?;
    let starved = Toolchain {
        space: STARVED_SPACE,
        ..Toolchain::new(image()?)
    };
    let outside =
        std::env::temp_dir().join(format!("nan-self-check-starved-{}", std::process::id()));
    let dir = outside.join("toolchain");
    copy_tree(&repository().join("toolchain"), &dir, false)?;
    let home = outside.join("home");
    let given = Given {
        home: &home,
        store: &store,
        asked: &[],
    };
    let (printed, code) = front(&starved, &dir, &given, &["check"])?.output()?;
    let expected = "internal-error check .: records 1\ninternal internal.resource-exhausted - -\n    reproducer .nan/repro/1\ninternal-error check .: records 1\n";
    if code != Some(3) || printed != expected.as_bytes() {
        return Err(format!(
            "the starved NaN toolchain exits with {code:?} and prints\n{}",
            String::from_utf8_lossy(&printed)
        )
        .into());
    }
    let reproducer = dir.join(".nan/repro/1");
    if fs::read_to_string(reproducer.join("command"))? != "check\n"
        || !reproducer.join("nan/package.nan").is_file()
    {
        return Err("the reproducer lacks the command line or the files the command read".into());
    }
    fs::remove_dir_all(outside)?;
    Ok(())
}
