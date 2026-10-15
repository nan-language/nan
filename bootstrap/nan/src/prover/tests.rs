use super::Status;
use crate::check::checked;
use crate::nir::Role;
use crate::workspace::tests::Scratch;
use crate::workspace::{Target, Workspace};

const MANIFEST: &str =
    "//! Specification example.\n\npackage example {\n    language 1\n    targets any\n} example\n";

/// The status of each obligation of an item's body, with its site's line.
fn statuses(files: &[(&str, &str)], item: &str) -> Vec<(u32, Status)> {
    let mut all = vec![("example/package.nan", MANIFEST)];
    all.extend_from_slice(files);
    let scratch = Scratch::new(&all);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let std = crate::stdlib::packages();
    let checker = checked(&workspace, std, Target::Any);
    let id = (0..checker.db.items.len())
        .map(|index| crate::check::db::ItemId(index as u32))
        .find(|id| !checker.db.is_std(*id) && checker.db.canonical(*id) == item)
        .expect("the item");
    let data = checker.db.item(id);
    checker
        .obligations(id)
        .iter()
        .filter(|site| site.role == Role::Body)
        .map(|site| (data.line(site.span.first), site.status))
        .collect()
}

#[test]
fn ensures_of_a_call_that_may_fail_hold_only_once_it_returns() {
    let source = "//! Counters.\n\npub closed enum Full derives Eq, Render {\n    Full\n} Full\n\nfn reset(mut count: u32) fails Full\n    ensures count == 0\n{\n    if count > 100 {\n        fail Full.Full\n    }\n    count = 0\n} reset\n\npub fn after(start: u32) -> u32 {\n    var count = start\n    let outcome = attempt reset(mut count)\n    return match outcome {\n        Ok(_) => count + 1\n        Err(_) => count + 1\n    }\n} after\n";
    let statuses = statuses(&[("example/example.nan", source)], "example::after");
    assert_eq!(statuses, vec![(20, Status::Proven), (21, Status::Checked)]);
}

#[test]
fn a_contract_naming_what_the_caller_cannot_see_is_opaque_to_it() {
    let meter = "//! A counter.\n\npub open struct Meter {\n    count: u32 where count <= 100\n} Meter\n\npub fn Meter.next(self) -> u32\n    ensures result == self.count + 1\n{\n    return self.count + 1\n} Meter.next\n";
    let root = "//! Its user.\n\npub fn after_next(m: meter::Meter) -> u32 {\n    return m.next() + 1\n} after_next\n";
    let statuses = statuses(
        &[
            ("example/meter/meter.nan", meter),
            ("example/example.nan", root),
        ],
        "example::after_next",
    );
    assert_eq!(statuses, vec![(4, Status::Checked)]);
}

#[test]
fn an_implementing_function_meets_and_gives_its_interface_ensures() {
    let source = "//! Tiles.\n\npub interface Sized {\n    fn size(self) -> u32\n        ensures result <= 100\n} Sized\n\npub closed struct Tile {\n    pub width: u32\n} Tile\n\nimpl Tile: Sized\n\npub fn Tile.size(self) -> u32 {\n    if self.width > 100 {\n        return 100\n    }\n    return self.width\n} Tile.size\n\npub fn padded(tile: Tile) -> u32 {\n    return tile.size() + 1\n} padded\n";
    let files = [("example/example.nan", source)];
    assert_eq!(
        statuses(&files, "example::Tile.size"),
        vec![(16, Status::Proven), (18, Status::Proven)]
    );
    assert_eq!(
        statuses(&files, "example::padded"),
        vec![(22, Status::Proven)]
    );
}

#[test]
fn a_reference_alternatives_pass_to_their_arm_is_the_part_it_designates() {
    let source = "//! Tokens.\n\npub closed struct Token {\n    pub size: u32\n} Token\n\npub closed struct Pair {\n    pub kind: u32\n    pub token: Token\n} Pair\n\nfn half(size: u32) -> u32\n    requires size <= 100\n{\n    return size / 2\n} half\n\npub fn half_size(pair: Pair) -> u32\n    requires pair.token.size <= 100\n{\n    return match pair {\n        Pair(kind: 0, token: token) or Pair(kind: 1, token: token) => half(token.size)\n        else => 0\n    }\n} half_size\n";
    assert_eq!(
        statuses(&[("example/example.nan", source)], "example::half_size"),
        vec![(22, Status::Proven)]
    );
}

#[test]
fn a_when_body_flows_into_the_code_around_it() {
    let source = "//! Counters.\n\npub closed struct Counter {\n    pub hits: u64\n} Counter\n\npub fn capped_hits(counter: task::Cown[Counter], cap: u64) -> u64 fails task::Cancelled uses wait\n    ensures result <= cap\n{\n    await when counter {\n        if counter.hits > cap {\n            return cap\n        }\n    }\n    return 0\n} capped_hits\n\npub fn stored(counter: task::Cown[Counter]) -> u32 fails task::Cancelled uses wait {\n    var seen: u32 = 0\n    await when counter {\n        seen = 7\n    }\n    expect seen == 7\n    return seen\n} stored\n\npub fn unstored(counter: task::Cown[Counter]) -> u32 fails task::Cancelled uses wait {\n    var seen: u32 = 0\n    await when counter {\n        seen = 7\n    }\n    expect seen == 0\n    return seen\n} unstored\n\npub fn early(counter: task::Cown[Counter], amount: u32) -> u32 fails task::Cancelled uses wait {\n    await when counter {\n        expect amount > 5\n    }\n    expect amount > 5\n    return amount\n} early\n";
    let files = [("example/example.nan", source)];
    assert_eq!(
        statuses(&files, "example::capped_hits"),
        vec![(15, Status::Proven), (12, Status::Proven)]
    );
    assert_eq!(
        statuses(&files, "example::stored"),
        vec![(23, Status::Proven)]
    );
    assert_eq!(
        statuses(&files, "example::unstored"),
        vec![(32, Status::Checked)]
    );
    assert_eq!(
        statuses(&files, "example::early"),
        vec![(38, Status::Checked), (40, Status::Checked)]
    );
}

#[test]
fn a_disequality_excludes_the_value_its_term_is_fixed_at() {
    let source = "//! Bytes.\n\npub fn head(bytes: Slice[u8]) -> u8 limits notrap {\n    if bytes.len() == 0 {\n        return 0\n    }\n    return bytes[0]\n} head\n";
    let statuses = statuses(&[("example/example.nan", source)], "example::head");
    assert_eq!(statuses, vec![(7, Status::Proven)]);
}

#[test]
fn where_conditions_of_const_parameters_are_facts_in_their_items_and_members() {
    let source = "//! Rings.\n\npub fn wrap[N: usize where N > 0](slots: Array[u8, N], index: usize) -> usize {\n    return index % N\n} wrap\n\npub closed struct Ring[N: usize where N > 0] {\n    pub slots: Array[u8, N]\n} Ring\n\npub fn Ring.wrap(self, index: usize) -> usize {\n    return index % N\n} Ring.wrap\n";
    let files = [("example/example.nan", source)];
    assert_eq!(statuses(&files, "example::wrap"), vec![(4, Status::Proven)]);
    assert_eq!(
        statuses(&files, "example::Ring.wrap"),
        vec![(12, Status::Proven)]
    );
}
