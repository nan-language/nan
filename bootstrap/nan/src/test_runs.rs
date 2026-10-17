//! Test runs: the tests a scope selects, the test image holding each package's tests for its
//! run target, each run as a process of that image, the failure each failing test reports, and
//! the pass record each passing test leaves in the build cache.

mod outcome;
mod setup;
mod shrink;

use std::collections::BTreeMap;
use std::{fs, io};

use crate::cache::{Cache, Exposed};
use crate::check::db::{ItemId, PackageId};
use crate::check::{self, Checker, testing};
use crate::diagnostic::Diagnostic;
use crate::encoding::{bytes, uvar};
use crate::hash::{Hash, blake3, checksummed_base32, read_checksummed_base32};
use crate::lower::test_image::{self, Target as Targeted};
use crate::output;
use crate::scope::Scope;
use crate::syntax::ast::ItemKind;
use crate::toolchain;
use crate::workspace::{Target, Workspace};
use outcome::{Ended, Run};

/// The most runs of a test without parameters: schedules 0 to 63.
const SCHEDULES: u64 = 64;

/// The runs of a property test: cases 0 to 255.
const CASES: u64 = 256;

/// The coverage class of every replay token.
const CLASS: &str = "sc";

/// A test that failed on a run target: its canonical path, the location of its head, its cause
/// and what its record holds.
pub struct Failure {
    pub item: String,
    pub path: String,
    pub line: u32,
    pub target: Target,
    pub cause: String,
    pub lines: Vec<String>,
}

/// What the runs of a scope's tests found: how many tests ran on a run target, the failures, and
/// what building their test images reports.
#[derive(Default)]
pub struct Report {
    pub tests: usize,
    pub failures: Vec<Failure>,
    pub diagnostics: Vec<Diagnostic>,
}

/// A test selected to run: its item, its target and whether it is a property test.
#[derive(Clone)]
struct Selected {
    item: ItemId,
    target: Targeted,
    property: bool,
}

/// Which runs of the tests a scope selects take place.
pub enum Runs {
    /// Every run of each test on each run target.
    All,
    /// Every run of each test on each run target where it is affected.
    Affected,
    /// The one run a replay token names.
    Replay(Token),
    /// Every run of each reliance test, and of each test whose reach holds an item of one of
    /// these packages.
    Reaching(Vec<String>),
}

/// The run a replay token names: its run target, its test's canonical path, its index and seed.
pub struct Token {
    target: Target,
    test: String,
    index: u64,
    seed: u64,
}

impl Token {
    /// The run a token names, when it parses and its class is `sc`.
    pub fn read(text: &str) -> Option<Token> {
        let parts: Vec<&str> = text.split('/').collect();
        let [class, target, test, index, seed] = parts[..] else {
            return None;
        };
        let parsed: u64 = index.parse().ok()?;
        let seed = read_checksummed_base32(seed, 8)?;
        (class == CLASS && parsed.to_string() == index).then_some(Token {
            target: Target::from_name(target).filter(|target| *target != Target::Any)?,
            test: test.to_owned(),
            index: parsed,
            seed: u64::from_le_bytes(seed.try_into().expect("eight bytes")),
        })
    }

    /// The location of the head of the test the token names, when it is a test of the workspace
    /// and the token names one of its run targets.
    pub fn location(&self, workspace: &Workspace) -> Option<String> {
        workspace.closures.iter().find_map(|closure| {
            let checker =
                check::unchecked(workspace, closure, crate::stdlib::packages(), Target::Host);
            let item = (0..checker.db.items.len())
                .map(|index| ItemId(index as u32))
                .find(|item| {
                    let data = checker.db.item(*item);
                    matches!(data.ast.kind, ItemKind::Test(_))
                        && checker.db.is_reported(*item)
                        && checker.db.canonical(*item) == self.test
                })?;
            let data = checker.db.item(item);
            run_targets(&checker, checker.db.package_of(item))
                .contains(&self.target)
                .then(|| format!("{}:{}", data.file.path, data.line(data.ast.token)))
        })
    }
}

/// Runs the tests a scope selects on each run target of their packages, as `runs` keeps them;
/// a part of the build cache that is exposed ends it before any run.
pub fn run(
    workspace: &Workspace,
    scope: &Scope,
    runs: &Runs,
    cache: &Cache,
) -> io::Result<Result<Report, Exposed>> {
    let mut report = Report::default();
    if !workspace.files().any(|(_, file)| {
        file.lowered()
            .iter()
            .flatten()
            .any(|item| matches!(item.kind, ItemKind::Test(_)))
    }) {
        return Ok(Ok(report));
    }
    let std = crate::stdlib::packages();
    for closure in &workspace.closures {
        let checker = check::checked(workspace, closure, std, Target::Host);
        match run_closure(&checker, workspace, scope, runs, cache, &mut report)? {
            Ok(()) => {}
            Err(exposed) => return Ok(Err(exposed)),
        }
    }
    report.failures.sort_by(|left, right| {
        (&left.path, left.line, left.target.name()).cmp(&(
            &right.path,
            right.line,
            right.target.name(),
        ))
    });
    Ok(Ok(report))
}

/// Runs the tests a scope selects among the packages a checker's closure checks.
fn run_closure(
    checker: &Checker,
    workspace: &Workspace,
    scope: &Scope,
    runs: &Runs,
    cache: &Cache,
    report: &mut Report,
) -> io::Result<Result<(), Exposed>> {
    let mut by_package: BTreeMap<u32, Vec<Selected>> = BTreeMap::new();
    for selected in select(checker, workspace, scope) {
        let package = checker.db.package_of(selected.item);
        by_package.entry(package.0).or_default().push(selected);
    }
    let mut planned = Vec::new();
    for (package, tests) in by_package {
        let package = PackageId(package);
        for target in run_targets(checker, package) {
            let kept = match runs {
                Runs::All => tests.clone(),
                Runs::Affected => {
                    let mut affected = Vec::new();
                    for test in &tests {
                        let record =
                            match cache.record(&record_name(checker, test.item, target))? {
                                Ok(record) => record,
                                Err(exposed) => return Ok(Err(exposed)),
                            };
                        if record != Some(pass_record(checker, test.item, target)) {
                            affected.push(test.clone());
                        }
                    }
                    affected
                }
                Runs::Replay(token) if token.target == target => tests.clone(),
                Runs::Replay(_) => Vec::new(),
                Runs::Reaching(packages) => tests
                    .iter()
                    .filter(|test| {
                        let foreign = |item: ItemId| checker.db.package_of(item) != package;
                        let reliance = match &checker.db.ast(test.item).kind {
                            ItemKind::Test(item) => matches!(
                                checker.test_target(test.item, item),
                                Some(testing::Target::Function(target) | testing::Target::Derived(target))
                                    if foreign(target)
                            ),
                            _ => false,
                        };
                        reliance
                            || checker.test_reach(test.item).into_iter().any(|item| {
                                let name = &checker.db.package(checker.db.package_of(item)).name;
                                packages.contains(name)
                            })
                    })
                    .cloned()
                    .collect(),
            };
            planned.push((package, target, kept));
        }
    }
    for (package, target, tests) in planned {
        report.tests += tests.len();
        match target {
            Target::Host => {
                run_host(checker, workspace, package, &tests, (runs, cache), report)?;
            }
            _ => {
                for test in &tests {
                    report.failures.push(failure(
                        checker,
                        test.item,
                        target,
                        "test.emulator-unavailable",
                        Vec::new(),
                    ));
                }
            }
        }
    }
    Ok(Ok(()))
}

/// The name of the cache record of a test's passes on a run target.
fn record_name(checker: &Checker, test: ItemId, target: Target) -> String {
    let mut key = Vec::new();
    bytes(&mut key, checker.db.canonical(test).as_bytes());
    bytes(&mut key, target.name().as_bytes());
    format!("pass-{}", Hash::of(&key).full())
}

/// The record of a test passing on a run target: the toolchain hash, the run target's name,
/// then the canonical path and item hash of each item of the test's reach, in path order.
fn pass_record(checker: &Checker, test: ItemId, target: Target) -> Vec<u8> {
    let mut reach: Vec<(String, [u8; 33])> = checker
        .test_reach(test)
        .into_iter()
        .map(|item| (checker.db.canonical(item), checker.hashes(item).item.0))
        .collect();
    reach.sort();
    let mut record = Vec::new();
    bytes(&mut record, &toolchain::hash(1).0);
    bytes(&mut record, target.name().as_bytes());
    uvar(&mut record, reach.len() as u128);
    for (path, hash) in reach {
        bytes(&mut record, path.as_bytes());
        bytes(&mut record, &hash);
    }
    record
}

/// The tests of a scope that run: of a package, module or file, every test in it; of a test,
/// that test; of another item, the tests targeting it; never a disputed test or a property test
/// with a generator gap.
fn select(checker: &Checker, workspace: &Workspace, scope: &Scope) -> Vec<Selected> {
    let targeted = match scope {
        Scope::Lines(path, lines) => item_at(checker, path, *lines.start())
            .filter(|item| !matches!(checker.db.ast(*item).kind, ItemKind::Test(_))),
        _ => None,
    };
    let mut selected = Vec::new();
    for index in 0..checker.db.items.len() {
        let item = ItemId(index as u32);
        let data = checker.db.item(item);
        let ItemKind::Test(test) = &data.ast.kind else {
            continue;
        };
        if test.disputed || !checker.db.is_reported(item) {
            continue;
        }
        let property = test.params.is_some();
        if property && checker.generation(item).is_none() {
            continue;
        }
        let Some(target) = checker.test_target(item, test) else {
            continue;
        };
        let wanted = match targeted {
            Some(scoped) => match target {
                testing::Target::Function(function) => function == scoped,
                testing::Target::Derived(owner) => owner == scoped,
            },
            None => scope.holds(workspace, &data.file.path, Some(data.line(data.ast.token))),
        };
        if !wanted {
            continue;
        }
        let target = match target {
            testing::Target::Function(function) => Targeted::Function(function),
            testing::Target::Derived(owner) => Targeted::Derived(
                owner,
                test.member
                    .as_ref()
                    .expect("a derived method's name")
                    .text
                    .clone(),
            ),
        };
        selected.push(Selected {
            item,
            target,
            property,
        });
    }
    selected
}

/// The item holding a line of a file.
fn item_at(checker: &Checker, path: &str, line: u32) -> Option<ItemId> {
    (0..checker.db.items.len())
        .map(|index| ItemId(index as u32))
        .find(|item| {
            let data = checker.db.item(*item);
            data.file.path == path
                && data
                    .syntax()
                    .item_lines(&data.syntax().items[data.index])
                    .contains(&line)
        })
}

/// The run targets of a package: each target it declares, `any` standing for `x86_64-host`.
fn run_targets(checker: &Checker, package: PackageId) -> Vec<Target> {
    let declared = checker
        .db
        .package(package)
        .source
        .manifest
        .as_ref()
        .map(|manifest| manifest.targets.clone())
        .unwrap_or_default();
    let mut targets: Vec<Target> = declared
        .into_iter()
        .map(|target| match target {
            Target::Any => Target::Host,
            other => other,
        })
        .collect();
    if targets.is_empty() {
        targets.push(Target::Host);
    }
    targets.sort();
    targets.dedup();
    targets
}

/// Runs a package's tests on `x86_64-host`, each run a process of the package's test image, and
/// records in the cache each test whose every run passes, unless replaying.
fn run_host(
    checker: &Checker,
    workspace: &Workspace,
    package: PackageId,
    tests: &[Selected],
    (runs, cache): (&Runs, &Cache),
    report: &mut Report,
) -> io::Result<()> {
    if tests.is_empty() {
        return Ok(());
    }
    let image = match build(checker, workspace, package, tests)? {
        Ok(image) => image,
        Err(collisions) => {
            report.diagnostics.extend(collisions);
            return Ok(());
        }
    };
    for (number, test) in tests.iter().enumerate() {
        let path = checker.db.canonical(test.item);
        let indexed: Vec<(u64, u64)> = match runs {
            Runs::Replay(token) => vec![(token.index, token.seed)],
            Runs::All | Runs::Affected | Runs::Reaching(_) => {
                let hash = checker.hashes(test.item).item;
                let count = if test.property { CASES } else { SCHEDULES };
                (0..count)
                    .map(|index| (index, seed(&hash, Target::Host, index)))
                    .collect()
            }
        };
        let failures = report.failures.len();
        for (index, seed) in indexed {
            let token = token(Target::Host, &path, index, seed);
            let run = outcome::run(&image, number, (index, seed), &token, None)?;
            let Some(cause) = cause(&run) else {
                if !test.property && !run.contended {
                    break;
                }
                continue;
            };
            let mut lines = Vec::new();
            let run = match test.property {
                true => {
                    let failed = signature(&run);
                    let shrunk = shrink::shrink(run, |forced| {
                        let candidate =
                            outcome::run(&image, number, (index, seed), &token, Some(forced))?;
                        Ok((signature(&candidate) == failed).then_some(candidate))
                    })?;
                    lines.extend(regression(checker, workspace, test.item, &shrunk));
                    shrunk
                }
                false => run,
            };
            let mut held = record_lines(&run, &token);
            let replay = held.pop();
            held.extend(lines);
            held.extend(replay);
            report
                .failures
                .push(failure(checker, test.item, Target::Host, &cause, held));
            break;
        }
        if report.failures.len() == failures && !matches!(runs, Runs::Replay(_)) {
            let name = record_name(checker, test.item, Target::Host);
            cache.replace(&name, &pass_record(checker, test.item, Target::Host))?;
        }
    }
    fs::remove_file(&image)
}

/// Lowers and links the test image of a package's tests, under `.nan/tests/`; or the symbol
/// collisions of its lowering.
fn build(
    checker: &Checker,
    workspace: &Workspace,
    package: PackageId,
    tests: &[Selected],
) -> io::Result<Result<std::path::PathBuf, Vec<Diagnostic>>> {
    let lowered = test_image::lower(
        checker,
        &test_image::Tests {
            tests: tests
                .iter()
                .map(|test| test_image::Test {
                    item: test.item,
                    target: test.target.clone(),
                })
                .collect(),
            roots: vec![package],
            prefixes: crate::final_build::prefixes(&crate::final_build::artefacts(
                workspace, checker, package,
            )),
        },
    );
    let data = checker.db.package(package);
    let collisions = crate::final_build::collisions(data.source, &lowered);
    if !collisions.is_empty() {
        return Ok(Err(collisions));
    }
    let image = workspace
        .root
        .join(".nan/tests")
        .join(Target::Host.name())
        .join(&data.name);
    crate::final_build::link(&lowered.object, &image).map_err(io::Error::other)?;
    Ok(Ok(image))
}

/// The seed of a run: the first 8 bytes, little-endian, of the BLAKE3-256 hash of the test's
/// item hash, the run target's name and the run's index.
fn seed(hash: &Hash, target: Target, index: u64) -> u64 {
    let mut input = hash.0.to_vec();
    input.extend_from_slice(target.name().as_bytes());
    input.extend_from_slice(&index.to_le_bytes());
    let digest = blake3(&input);
    u64::from_le_bytes(digest[..8].try_into().expect("eight bytes"))
}

/// The replay token naming a run.
fn token(target: Target, test: &str, index: u64, seed: u64) -> String {
    format!(
        "{CLASS}/{}/{test}/{index}/{}",
        target.name(),
        checksummed_base32(&seed.to_le_bytes())
    )
}

/// The cause of a failing run, the first that applies of a trap, an escaped failure, a target
/// that did not run, a stall and a timeout; none for a passing run.
fn cause(run: &Run) -> Option<String> {
    Some(match &run.ended {
        Ended::Passed { ran: true } => return None,
        Ended::Passed { ran: false } | Ended::Stalled { ran: false } => {
            "test.target-not-run".to_owned()
        }
        Ended::Trapped { report } => report
            .split(' ')
            .next()
            .expect("a trap report's kind")
            .to_owned(),
        Ended::Escaped { .. } => "test.escaped-failure".to_owned(),
        Ended::Stalled { ran: true } => "test.stalled".to_owned(),
        Ended::TimedOut => "test.timeout".to_owned(),
    })
}

/// How a run failed, for keeping a candidate that fails the same way: its trap kind and the
/// location of the trapping operation, its escaped failure's variant, or its outcome code.
fn signature(run: &Run) -> Option<String> {
    match &run.ended {
        Ended::Trapped { report } => report
            .split_once(" in ")
            .map(|(kind_and_location, _)| kind_and_location.to_owned()),
        Ended::Escaped { path, .. } => Some(path.clone()),
        _ => cause(run),
    }
}

/// The setup forms of a shrunk case's parameters and the lines of its regression-test edit,
/// offered when the edited workspace reports no error in the test it inserts.
fn regression(checker: &Checker, workspace: &Workspace, test: ItemId, shrunk: &Run) -> Vec<String> {
    let given: Vec<i128> = shrunk.draws.iter().map(|draw| draw.given).collect();
    let (statements, fix) = setup::regression(checker, test, &given);
    let mut lines = vec!["setup".to_owned()];
    lines.extend(
        statements
            .iter()
            .map(|statement| format!("    {}", output::printable(statement.as_bytes()))),
    );
    let after = crate::fix::fixed(workspace, &fix);
    let (path, inserted) = &fix.0[0];
    let first = workspace.file(path).map_or(0, |(_, file)| {
        file.syntax
            .lines
            .partition_point(|line| line.start <= inserted.start) as u32
    });
    let count = inserted.text.lines().count() as u32;
    let erroneous = check::reported(&after, &Scope::Workspace)
        .iter()
        .any(|diagnostic| {
            diagnostic.code.class() == crate::diagnostic::Class::Error
                && diagnostic.path.as_deref() == Some(path.as_str())
                && diagnostic
                    .line
                    .is_some_and(|line| line >= first && line <= first + count)
        });
    if !erroneous {
        lines.extend(crate::fix::shown(workspace, &[fix]));
    }
    lines
}

/// What the record of a failing run holds: its trap report or escaped failure, its test trace
/// and its replay token.
fn record_lines(run: &Run, token: &str) -> Vec<String> {
    let mut lines = Vec::new();
    match &run.ended {
        Ended::Trapped { report } => lines.extend(report.lines().map(str::to_owned)),
        Ended::Escaped { path, rendering } => {
            lines.push(format!("failure {path}"));
            lines.extend(
                rendering
                    .lines()
                    .map(|line| format!("    {}", output::printable(line.as_bytes()))),
            );
        }
        _ => {}
    }
    if !run.trace.is_empty() {
        lines.push("trace".to_owned());
        lines.extend(
            String::from_utf8_lossy(&run.trace)
                .lines()
                .map(|line| format!("    {}", output::printable(line.as_bytes()))),
        );
    }
    lines.push(format!("replay {token}"));
    lines
}

/// The failure of a test on a run target, at its head.
fn failure(
    checker: &Checker,
    item: ItemId,
    target: Target,
    cause: &str,
    lines: Vec<String>,
) -> Failure {
    let data = checker.db.item(item);
    Failure {
        item: checker.db.canonical(item),
        path: data.file.path.clone(),
        line: data.line(data.ast.token),
        target,
        cause: cause.to_owned(),
        lines,
    }
}
