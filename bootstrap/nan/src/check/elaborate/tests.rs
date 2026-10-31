use crate::check::db::{ItemId, PackageId, TypeKey};
use crate::check::resolve::is_core_vocabulary;
use crate::check::sig::SigKind;
use crate::check::ty::{Arg, Const, Param as GenericParam, Ty};
use crate::check::{Checker, checked};
use crate::syntax::ast::Mode;
use crate::workspace::tests::Scratch;
use crate::workspace::{Target, Workspace};

const MANIFEST: &str =
    "//! Specification example.\n\npackage example {\n    language 1\n    targets any\n} example\n";

/// A one-file package holding `source`.
fn scratch(source: &str) -> Scratch {
    let file = format!("//! Specification example.\n\n{source}");
    Scratch::new(&[
        ("example/package.nan", MANIFEST),
        ("example/example.nan", &file),
    ])
}

/// The NIR of an item of a one-file package, in the text notation.
fn unit_text(source: &str, item: &str) -> String {
    unit_of(source, item).expect("a unit")
}

/// The NIR of an item of a one-file package in the text notation, when the item has a unit.
fn unit_of(source: &str, item: &str) -> Option<String> {
    let scratch = scratch(source);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let std = crate::stdlib::packages();
    let checker = checked(&workspace, &workspace.closures[0], std, Target::Any);
    let id = (0..checker.db.items.len())
        .map(|index| crate::check::db::ItemId(index as u32))
        .find(|id| !checker.db.is_std(*id) && checker.db.canonical(*id) == item)
        .expect("the item");
    let unit = checker.unit(id)?;
    let names = UnitNames {
        ck: &checker,
        package: checker.db.package_of(id),
    };
    Some(crate::nir::written(&unit, &names))
}

#[test]
fn an_ensures_clause_is_checked_at_each_return() {
    let text = unit_text(
        "pub fn clamp_percent(value: u32) -> u8\n    ensures result <= 100\n{\n    if value > 100 {\n        return 100\n    }\n    return u8.fit(value)\n} clamp_percent\n",
        "example::clamp_percent",
    );
    assert_eq!(
        text,
        "unit example::clamp_percent\n\nfn body (read %0: u32) -> u8\n^0:\n    %1 = gt %0, 100u32\n    branch %1, ^1, ^2\n^1:\n    %2 = call example::clamp_percent ensures 0(%0, 100u8)\n    check ensures, %2\n    return 100u8\n^2:\n    %3 = call u8.fit(%0)\n    %4 = call example::clamp_percent ensures 0(%0, %3)\n    check ensures, %4\n    return %3\n\nfn ensures 0 (read %0: u32, read %1: u8) -> bool\n^0:\n    %2 = le %1, 100u8\n    return %2\n"
    );
}

#[test]
fn an_operation_call_checks_the_requires_of_its_operation_over_the_arguments_after_the_outbox() {
    let source = "pub protocol Cell {\n    pub fn write(value: u32) limits pure\n        requires value > 0\n} Cell\n\npub fn fill(inbox: sys::Outbox[Cell], value: u32) fails task::SendError uses wait {\n    try await inbox.write(value: value)\n} fill\n";
    assert_eq!(
        unit_text(source, "example::fill"),
        "unit example::fill\n\nfn body (read %0: sys::Outbox[example::Cell], read %1: u32) fails task::SendError uses wait\n^0:\n    %2 = call example::Cell requires 0(%1)\n    check requires, %2\n    call await example::Cell.write[sys::Outbox[example::Cell]](%0, %1) ok ^1 fail ^2\n^1(%3: Unit):\n    return\n^2(%4: task::SendError):\n    fail %4\n"
    );
}

#[test]
fn a_failure_exit_checks_no_predicate_of_the_result_type() {
    let text = unit_text(
        "pub closed struct Level {\n    pub n: u8 where n >= 2\n} Level\n\npub closed enum Low {\n    Below\n} Low\n\npub fn level(n: u8) -> Level fails Low {\n    if n < 2 {\n        fail Low.Below\n    }\n    return Level(n: n)\n} level\n",
        "example::level",
    );
    assert_eq!(
        text,
        "unit example::level\n\nfn body (read %0: u8) -> example::Level fails example::Low\n^0:\n    %1 = lt %0, 2u8\n    branch %1, ^1, ^2\n^1:\n    %2 = variant example::Low.Below\n    fail %2\n^2:\n    %3 = construct example::Level(n: %0)\n    %4 = call example::Level predicate 0(%3)\n    check predicate, %4\n    %5 = call example::Level predicate 0(%3)\n    check predicate, %5\n    return %3\n"
    );
}

#[test]
fn a_for_loop_holds_its_cursor_in_a_slot() {
    let text = unit_text(
        "pub fn total(values: Slice[u32]) -> u64 {\n    var sum: u64 = 0\n    for value in values {\n        sum += u64.widen(value)\n    }\n    return sum\n} total\n",
        "example::total",
    );
    assert_eq!(
        text,
        "unit example::total\n\nfn body (read %0: Slice[u32]) -> u64\n^0:\n    %1 = slot sum: u64\n    init %1, 0u64\n    %2 = slot core::SliceCursor[u32]\n    %3 = call Slice.cursor[u32](%0)\n    init %2, %3\n    jump ^1\n^1:\n    %4 = call core::SliceCursor.next[u32](mut %2)\n    %5 = is %4, Option.Some\n    branch %5, ^2, ^3\n^2:\n    %6 = load %4.Some\n    %7 = call u64.widen(%6)\n    %8 = load %1\n    %9 = add %8, %7\n    assign %1, %9\n    jump ^1\n^3:\n    %10 = load %1\n    return %10\n"
    );
}

#[test]
fn a_for_loop_over_a_registry_of_an_unknown_type_has_no_unit() {
    let source = "pub registry commands: Command keyed by opcode\n\npub fn count() -> u32 {\n    var total: u32 = 0\n    for command in commands {\n        total = total.saturating_add(1)\n    }\n    return total\n} count\n";
    assert_eq!(unit_of(source, "example::count"), None);
}

const LABELS: &str = "pub closed struct Token {\n    pub size: u32\n} Token\n\npub closed enum Label {\n    Short(Token)\n    Long(Token)\n    Blank\n} Label\n\npub closed struct Pair {\n    pub kind: u32\n    pub token: Token\n} Pair\n\nconst LIMIT: u32 = 2 * 5\n\n";

#[test]
fn alternatives_move_into_the_slot_their_arm_creates_before_the_first_test() {
    let source = format!(
        "{LABELS}pub fn size_of(take label: Label) -> u32 {{\n    return match label {{\n        Label.Short(token) or Label.Long(token) => token.size\n        Label.Blank => 0\n    }}\n}} size_of\n"
    );
    assert_eq!(
        unit_text(&source, "example::size_of"),
        "unit example::size_of\n\nfn body (take %0: example::Label) -> u32\n^0:\n    %1 = slot label: example::Label\n    init %1, %0\n    %2 = slot token: example::Token\n    %3 = is %1, example::Label.Short\n    branch %3, ^1, ^4\n^1:\n    %4 = move %1.Short\n    init %2, %4\n    jump ^2\n^2:\n    %5 = load %2.size\n    release %2\n    jump ^3(%5)\n^3(%6: u32):\n    return %6\n^4:\n    %7 = is %1, example::Label.Long\n    branch %7, ^5, ^6\n^5:\n    %8 = move %1.Long\n    init %2, %8\n    jump ^2\n^6:\n    release %1\n    jump ^3(0u32)\n"
    );
}

#[test]
fn a_guard_reads_by_reference_what_its_pattern_moves_after_it() {
    let source = format!(
        "{LABELS}pub fn weight(take pair: Pair) -> u32 {{\n    return match first pair {{\n        Pair(kind: 0..<LIMIT, token: token) if token.size > 0 => token.size\n        else => LIMIT\n    }}\n}} weight\n"
    );
    assert_eq!(
        unit_text(&source, "example::weight"),
        "unit example::weight\n\nfn body (take %0: example::Pair) -> u32\n^0:\n    %1 = slot pair: example::Pair\n    init %1, %0\n    %2 = load %1.kind\n    %3 = ge %2, 0u32\n    branch %3, ^1, ^5\n^1:\n    %4 = constant example::LIMIT\n    %5 = lt %2, %4\n    branch %5, ^2, ^5\n^2:\n    %6 = ref %1.token\n    %7 = load %6.size\n    %8 = gt %7, 0u32\n    branch %8, ^3, ^5\n^3:\n    %9 = slot token: example::Token\n    %10 = move %1.token\n    init %9, %10\n    %11 = load %9.size\n    release %9\n    jump ^4(%11)\n^4(%12: u32):\n    return %12\n^5:\n    %13 = constant example::LIMIT\n    release %1.token\n    jump ^4(%13)\n"
    );
}

#[test]
fn matches_run_as_their_arms_test_and_bind() {
    let source = format!(
        "{LABELS}pub fn size_of(label: Label) -> u32\n    example size_of(Label.Long(Token(size: 4))) == 4\n    example size_of(Label.Blank) == 0\n{{\n    return match label {{\n        Label.Short(token) or Label.Long(token) => token.size\n        Label.Blank => 0\n    }}\n}} size_of\n\npub fn owned_size(take label: Label) -> u32\n    example owned_size(Label.Long(Token(size: 4))) == 4\n    example owned_size(Label.Blank) == 0\n{{\n    return match label {{\n        Label.Short(token) or Label.Long(token) => token.size\n        Label.Blank => 0\n    }}\n}} owned_size\n\npub fn guarded(take label: Label) -> u32\n    example guarded(Label.Short(Token(size: 3))) == 3\n    example guarded(Label.Short(Token(size: 0))) == 100\n    example guarded(Label.Blank) == 0\n{{\n    return match first label {{\n        Label.Short(token) if token.size > 0 => token.size\n        Label.Short(token) => 100 + token.size\n        else => 0\n    }}\n}} guarded\n\npub fn guarded_alternatives(take label: Label) -> u32\n    example guarded_alternatives(Label.Long(Token(size: 3))) == 3\n    example guarded_alternatives(Label.Short(Token(size: 0))) == 7\n{{\n    return match first label {{\n        Label.Short(token) or Label.Long(token) if token.size > 0 => token.size\n        else => 7\n    }}\n}} guarded_alternatives\n\npub fn band(value: u32) -> u8\n    example band(9) == 0\n    example band(10) == 1\n    example band(11) == 2\n{{\n    return match first value {{\n        0..<LIMIT => 0\n        LIMIT => 1\n        else => 2\n    }}\n}} band\n"
    );
    let scratch = scratch(&source);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let reported: Vec<_> = crate::check::reported(&workspace, &crate::scope::Scope::Workspace)
        .into_iter()
        .map(|diagnostic| (diagnostic.line, diagnostic.code))
        .collect();
    assert_eq!(reported, Vec::new());
}

#[test]
fn temporaries_of_a_condition_or_branch_end_with_it() {
    let source = format!(
        "{LABELS}pub fn make(size: u32) -> Token = Token(size: size)\n\npub fn pick(flag: bool, bound: u32) -> u32 {{\n    var index: u32 = 0\n    while make(index).size < bound {{\n        index += 1\n    }}\n    return match flag {{\n        true => make(index).size\n        false => 0\n    }}\n}} pick\n"
    );
    assert_eq!(
        unit_text(&source, "example::pick"),
        "unit example::pick\n\nfn body (read %0: bool, read %1: u32) -> u32\n^0:\n    %2 = slot index: u32\n    init %2, 0u32\n    jump ^1\n^1:\n    %3 = load %2\n    %4 = call example::make(%3)\n    %5 = slot example::Token\n    init %5, %4\n    %6 = load %5.size\n    %7 = lt %6, %1\n    release %5\n    branch %7, ^2, ^3\n^2:\n    %8 = load %2\n    %9 = add %8, 1u32\n    assign %2, %9\n    jump ^1\n^3:\n    %10 = eq %0, true\n    branch %10, ^4, ^6\n^4:\n    %11 = load %2\n    %12 = call example::make(%11)\n    %13 = slot example::Token\n    init %13, %12\n    %14 = load %13.size\n    release %13\n    jump ^5(%14)\n^5(%15: u32):\n    return %15\n^6:\n    jump ^5(0u32)\n"
    );
}

#[test]
fn some_of_a_part_refers_to_the_part_and_releases_nothing() {
    let text = unit_text(
        "pub closed struct Frame {\n    pub seq: u32\n    pub payload: mem::List[u8]\n} Frame\n\npub fn seq_or_zero(frame: Option[Frame]) -> u32 {\n    let Some(found) = frame else {\n        return 0\n    }\n    return found.seq\n} seq_or_zero\n\npub fn first_seq(frames: mem::List[Frame]) -> u32 {\n    if frames.len() == 0 {\n        return 0\n    }\n    return seq_or_zero(Some(frames.at(0)))\n} first_seq\n",
        "example::first_seq",
    );
    assert_eq!(
        text,
        "unit example::first_seq\n\nfn body (read %0: ref mem::List[example::Frame]) -> u32\n^0:\n    %1 = call mem::List.len[example::Frame](read %0)\n    %2 = eq %1, 0usize\n    branch %2, ^1, ^2\n^1:\n    return 0u32\n^2:\n    %3 = call mem::List.at[example::Frame](read %0, 0usize)\n    %4 = variant Option.Some(read %3)\n    %5 = call example::seq_or_zero(%4)\n    return %5\n"
    );
}

#[test]
fn a_discarded_part_stays_with_its_owner() {
    let text = unit_text(
        "pub fn first_len(lists: mem::List[mem::List[u8]]) -> usize {\n    if lists.len() == 0 {\n        return 0\n    }\n    discard lists.at(0)\n    return lists.len()\n} first_len\n",
        "example::first_len",
    );
    assert_eq!(
        text,
        "unit example::first_len\n\nfn body (read %0: ref mem::List[mem::List[u8]]) -> usize\n^0:\n    %1 = call mem::List.len[mem::List[u8]](read %0)\n    %2 = eq %1, 0usize\n    branch %2, ^1, ^2\n^1:\n    return 0usize\n^2:\n    %3 = call mem::List.at[mem::List[u8]](read %0, 0usize)\n    %4 = call mem::List.len[mem::List[u8]](read %0)\n    return %4\n"
    );
}

#[test]
fn an_expression_body_gives_a_part_in_place() {
    let text = unit_text(
        "pub closed struct Frame {\n    pub seq: u32\n    pub payload: mem::List[u8]\n} Frame\n\n/// The frame itself.\npub proj fn Frame.as_part(self) -> Frame = self\n",
        "example::Frame.as_part",
    );
    assert_eq!(
        text,
        "unit example::Frame.as_part\n\nfn body (read %0: ref example::Frame) -> example::Frame\n^0:\n    return take %0\n"
    );
}

#[test]
fn an_exit_releases_the_temporaries_of_the_statements_it_leaves() {
    let source = format!(
        "{LABELS}pub fn make(size: u32) -> Token = Token(size: size)\n\npub fn early(flag: bool) -> u32 {{\n    match Label.Short(make(3)) {{\n        Label.Short(token) => {{\n            if flag {{\n                return make(token.size).size\n            }}\n        }}\n        else => {{}}\n    }}\n    return 0\n}} early\n"
    );
    assert_eq!(
        unit_text(&source, "example::early"),
        "unit example::early\n\nfn body (read %0: bool) -> u32\n^0:\n    %1 = call example::make(3u32)\n    %2 = variant example::Label.Short(%1)\n    %3 = slot example::Label\n    init %3, %2\n    %4 = is %3, example::Label.Short\n    branch %4, ^1, ^5\n^1:\n    %5 = slot token: example::Token\n    %6 = move %3.Short\n    init %5, %6\n    branch %0, ^2, ^3\n^2:\n    %7 = load %5.size\n    %8 = call example::make(%7)\n    %9 = slot example::Token\n    init %9, %8\n    %10 = load %9.size\n    release %9\n    release %5\n    return %10\n^3:\n    release %5\n    jump ^4\n^4:\n    return 0u32\n^5:\n    release %3\n    jump ^4\n"
    );
}

#[test]
fn an_exit_leaves_a_when_body_by_a_jump_out_of_its_region() {
    let text = unit_text(
        "pub closed struct Counter {\n    pub hits: u64\n} Counter\n\npub fn capped_hits(counter: task::Cown[Counter], cap: u64) -> u64 fails task::Cancelled uses wait\n    ensures result <= cap\n{\n    await when counter {\n        if counter.hits > cap {\n            return cap\n        }\n    }\n    return 0\n} capped_hits\n",
        "example::capped_hits",
    );
    assert_eq!(
        text,
        "unit example::capped_hits\n\nfn body (read %0: ref task::Cown[example::Counter], read %1: u64) -> u64 fails task::Cancelled uses wait\n^0:\n    when (read %0) {\n    ^1(%2: ref example::Counter):\n        %3 = load %2.hits\n        %4 = gt %3, %1\n        branch %4, ^2, ^4\n    ^2:\n        jump ^3(%1)\n    ^4:\n        yield Unit\n    }\n    %7 = call example::capped_hits ensures 0(read %0, %1, 0u64)\n    check ensures, %7\n    return 0u64\n^3(%5: u64):\n    %6 = call example::capped_hits ensures 0(read %0, %1, %5)\n    check ensures, %6\n    return %5\n\nfn ensures 0 (read %0: ref task::Cown[example::Counter], read %1: u64, read %2: u64) -> bool\n^0:\n    %3 = le %2, %1\n    return %3\n"
    );
}

/// How the NIR of a package's units writes the items, types and generic parameters it names:
/// in alias form, as that package's source would.
struct UnitNames<'n, 'a> {
    ck: &'n Checker<'a>,
    package: PackageId,
}

impl crate::nir::Names for UnitNames<'_, '_> {
    fn path(&self, item: ItemId) -> String {
        let db = &self.ck.db;
        let name = db.declaration(item).name().unwrap_or_default();
        if let Some(owner) = db.owners.get(&item) {
            let member = name
                .rsplit_once('.')
                .map_or(name.as_str(), |(_, member)| member);
            let owner = match owner {
                TypeKey::Item(owner) => self.path(*owner),
                TypeKey::Scalar(scalar) => scalar.name().to_owned(),
                TypeKey::Unit => "Unit".to_owned(),
            };
            return format!("{owner}.{member}");
        }
        let module = db.module_of(item);
        let package = db.module(module).package;
        if db.is_std(item) && is_core_vocabulary(&name) {
            return name;
        }
        let canonical = db.canonical(item);
        if package == self.package || db.is_std(item) {
            return canonical;
        }
        let alias = db
            .package(self.package)
            .deps
            .iter()
            .find(|(_, target)| **target == Some(package))
            .map(|(alias, _)| alias.clone());
        match (alias, canonical.split_once("::")) {
            (Some(alias), Some((_, rest))) => format!("{alias}::{rest}"),
            _ => canonical,
        }
    }

    fn ty(&self, ty: &Ty) -> String {
        let args = |args: &[Arg]| -> String {
            if args.is_empty() {
                return String::new();
            }
            let written: Vec<String> = args
                .iter()
                .map(|arg| match arg {
                    Arg::Ty(ty) => self.ty(ty),
                    Arg::Const(Const::Int(value)) => value.to_string(),
                    Arg::Const(Const::Bool(value)) => value.to_string(),
                    Arg::Const(Const::Param(param)) => self.param(*param),
                    Arg::Const(Const::Error) => "_".to_owned(),
                })
                .collect();
            format!("[{}]", written.join(", "))
        };
        match ty {
            Ty::Scalar(scalar) => scalar.name().to_owned(),
            Ty::Unit => "Unit".to_owned(),
            Ty::Never => "Never".to_owned(),
            Ty::Error => "_".to_owned(),
            Ty::Item(item, arguments) => format!("{}{}", self.path(*item), args(arguments)),
            Ty::Registers(item) => self.path(*item),
            Ty::Param(param) => self.param(*param),
            Ty::Dyn(bound) => format!("Dyn[{}{}]", self.path(bound.interface), args(&bound.args)),
            Ty::Fn(function) => {
                let params: Vec<String> = function
                    .params
                    .iter()
                    .map(|param| {
                        let mode = match param.mode {
                            Mode::Read => "",
                            Mode::Mut => "mut ",
                            Mode::Take => "take ",
                        };
                        format!("{mode}{}: {}", param.name, self.ty(&param.ty))
                    })
                    .collect();
                let mut text = format!("fn({})", params.join(", "));
                if function.result != Ty::Unit {
                    text.push_str(&format!(" -> {}", self.ty(&function.result)));
                }
                text
            }
        }
    }

    fn param(&self, param: GenericParam) -> String {
        self.ck.generic_name(param)
    }

    fn method(&self, interface: ItemId, method: u32) -> String {
        self.ck.method_name(interface, method as usize)
    }

    fn register(&self, block: ItemId, register: u32) -> String {
        match &self.ck.sig(block).kind {
            SigKind::Registers { registers } => registers[register as usize].name.clone(),
            _ => "_".to_owned(),
        }
    }
}

const SCALED: &str = "pub interface Scaled[U: Copy] {\n    fn unit(self, base: U) -> u64\n    fn times[T: Copy](self, base: U, tag: T) -> u64 {\n        return self.unit(base)\n    } times\n    fn checked[T: Copy](self, count: u64, tag: T) -> u64\n        requires count > 0\n} Scaled\n\npub closed struct Meter derives Copy {\n    pub size: u64\n} Meter\n\nimpl Meter: Scaled[u8]\n\npub fn Meter.unit(self, base: u8) -> u64 = self.size\n\npub fn Meter.checked[T: Copy](self, count: u64, tag: T) -> u64 = self.size\n\npub fn total(meter: Meter) -> u64 {\n    return meter.times(1, tag: true)\n} total\n\npub fn bounded[S: Scaled[u8]](scaled: S) -> u64 {\n    return scaled.checked(1, tag: 5u16)\n} bounded\n";

#[test]
fn a_method_call_gives_the_method_its_own_type_arguments() {
    assert_eq!(
        unit_text(SCALED, "example::total"),
        "unit example::total\n\nfn body (read %0: example::Meter) -> u64\n^0:\n    %1 = call example::Scaled default 0[u8, example::Meter, bool](%0, 1u8, true)\n    return %1\n"
    );
    assert_eq!(
        unit_text(SCALED, "example::bounded"),
        "unit example::bounded\n\nfn body (read %0: ref S) -> u64\n^0:\n    %1 = call example::Scaled requires 0[u8, S, u16](read %0, 1u64, 5u16)\n    check requires, %1\n    %2 = call example::Scaled.checked[S, u8, u16](read %0, 1u64, 5u16)\n    return %2\n"
    );
}

#[test]
fn a_temporary_releases_only_what_its_pattern_leaves() {
    let text = unit_text(
        "pub fn made(alloc: mem::Alloc) -> Option[mem::Bytes] = Some(mem::Bytes.empty(alloc))\n\npub fn length(alloc: mem::Alloc, wanted: bool) -> usize {\n    var total: usize = 0\n    if wanted {\n        let Some(bytes) = made(alloc) else {\n            return 0\n        }\n        total = bytes.len()\n    }\n    return total\n} length\n",
        "example::length",
    );
    assert_eq!(
        text,
        "unit example::length\n\nfn body (read %0: mem::Alloc, read %1: bool) -> usize\n^0:\n    %2 = slot total: usize\n    init %2, 0usize\n    branch %1, ^1, ^3\n^1:\n    %3 = call example::made(%0)\n    %4 = slot Option[mem::Bytes]\n    init %4, %3\n    %5 = is %4, Option.Some\n    branch %5, ^2, ^4\n^2:\n    %6 = slot bytes: mem::Bytes\n    %7 = move %4.Some\n    init %6, %7\n    %8 = call mem::Bytes.len(read %6)\n    assign %2, %8\n    release %6\n    jump ^3\n^3:\n    %9 = load %2\n    return %9\n^4:\n    release %4\n    return 0usize\n"
    );
}

#[test]
fn a_failing_initialiser_releases_only_the_bindings_before_it() {
    let text = unit_text(
        "pub closed enum Fault {\n    Bad\n} Fault\n\npub fn made() -> mem::Bytes fails Fault {\n    fail Fault.Bad\n} made\n\npub fn both() -> usize fails Fault {\n    let one = try made()\n    let two = try made()\n    return one.len() + two.len()\n} both\n",
        "example::both",
    );
    assert_eq!(
        text,
        "unit example::both\n\nfn body () -> usize fails example::Fault\n^0:\n    %0 = slot one: mem::Bytes\n    call example::made() ok ^1 fail ^4\n^1(%1: mem::Bytes):\n    init %0, %1\n    %2 = slot two: mem::Bytes\n    call example::made() ok ^2 fail ^3\n^2(%3: mem::Bytes):\n    init %2, %3\n    %4 = call mem::Bytes.len(read %0)\n    %5 = call mem::Bytes.len(read %2)\n    %6 = add %4, %5\n    release %2\n    release %0\n    return %6\n^3(%7: example::Fault):\n    release %0\n    fail %7\n^4(%8: example::Fault):\n    fail %8\n"
    );
}

#[test]
fn a_forwarded_call_elaborates_as_its_rewrite_over_its_arguments() {
    let source = "pub closed struct Counter {\n    pub hits: u64\n} Counter\n\npub fn Counter.add(mut self, step: u64) {\n    self.hits = self.hits.saturating_add(step)\n} Counter.add\n\nretired Counter.bump(mut self, step: u32) => mut self.add(u64.widen(step)) preserves\n\npub fn tally(mut counter: Counter, step: u32) {\n    mut counter.bump(step)\n} tally\n";
    assert_eq!(
        unit_text(source, "example::tally"),
        "unit example::tally\n\nfn body (mut %0: ref mut example::Counter, read %1: u32)\n^0:\n    %2 = call u64.widen(%1)\n    call example::Counter.add(mut %0, %2)\n    return\n"
    );
}

const SHELF: &str = "pub closed struct Gauge {\n    pub level: u32\n} Gauge\n\npub proj fn Gauge.level_of(self) -> u32 = self.level\n\npub closed struct Shelf {\n    pub books: mem::List[u32]\n    pub gauge: Gauge\n} Shelf\n\npub proj fn Shelf.gauge_of(self) -> Gauge = self.gauge\n\npub fn made(alloc: mem::Alloc) -> Shelf = Shelf(books: mem::List[u32].empty(alloc), gauge: Gauge(level: 3))\n\n";

#[test]
fn a_projection_reads_its_receiver_place_in_place() {
    let source = format!("{SHELF}pub fn level(shelf: Shelf) -> u32 = shelf.gauge.level_of()\n");
    assert_eq!(
        unit_text(&source, "example::level"),
        "unit example::level\n\nfn body (read %0: ref example::Shelf) -> u32\n^0:\n    %1 = call example::Gauge.level_of(read %0.gauge)\n    %2 = load %1\n    return %2\n"
    );
}

#[test]
fn a_projection_releases_its_temporary_receiver() {
    let source =
        format!("{SHELF}pub fn fresh(alloc: mem::Alloc) -> u32 = made(alloc).gauge_of().level\n");
    assert_eq!(
        unit_text(&source, "example::fresh"),
        "unit example::fresh\n\nfn body (read %0: mem::Alloc) -> u32\n^0:\n    %1 = call example::made(%0)\n    %2 = slot example::Shelf\n    init %2, %1\n    %3 = call example::Shelf.gauge_of(read %2)\n    %4 = load %3.level\n    release %2\n    return %4\n"
    );
}

#[test]
fn a_call_failing_with_never_has_no_failure_path() {
    let source = "pub fn checked[E](value: u32, body: fn(input: u32) -> u32 fails E) -> u32 fails E {\n    return try body(value)\n} checked\n\npub fn doubled(value: u32) -> u32 {\n    return checked(value, body: fn(input: u32) -> u32 {\n        return input * 2\n    })\n} doubled\n";
    assert_eq!(
        unit_text(source, "example::doubled"),
        "unit example::doubled\n\nfn body (read %0: u32) -> u32\n^0:\n    %1 = closure 0()\n    %2 = call example::checked[Never](%0, %1)\n    return %2\n\nfn closure 0 (read %0: u32) -> u32\n^0:\n    %1 = mul %0, 2u32\n    return %1\n"
    );
}

#[test]
fn an_invariant_of_a_loop_in_a_nested_block_is_numbered() {
    let text = unit_text(
        "pub fn bounded(limit: u32) -> u32 {\n    var inner: u32 = 0\n    if limit < 5 {\n        while inner < limit {\n            invariant inner <= limit\n            inner += 1\n        }\n    }\n    return inner\n} bounded\n",
        "example::bounded",
    );
    assert_eq!(
        text,
        "unit example::bounded\n\nfn body (read %0: u32) -> u32\n^0:\n    %1 = slot inner: u32\n    init %1, 0u32\n    %2 = lt %0, 5u32\n    branch %2, ^1, ^5\n^1:\n    jump ^2\n^2:\n    %3 = load %1\n    %4 = lt %3, %0\n    branch %4, ^3, ^4\n^3:\n    %5 = load %1\n    %6 = call example::bounded invariant 0(%5, %0)\n    check invariant, %6\n    %7 = load %1\n    %8 = add %7, 1u32\n    assign %1, %8\n    jump ^2\n^4:\n    jump ^5\n^5:\n    %9 = load %1\n    return %9\n\nfn invariant 0 (read %0: u32, read %1: u32) -> bool\n^0:\n    %2 = le %0, %1\n    return %2\n"
    );
}
