use crate::check::reported;
use crate::scope::Scope;
use crate::workspace::Workspace;
use crate::workspace::tests::Scratch;

const MANIFEST: &str =
    "//! Hash cases.\n\npackage app {\n    language 1\n    targets x86_64-host\n} app\n";

const CASES: &str = r#"//! Hash cases.

pub closed struct Sum {
    pub total: u64
    pub writes: u32
} Sum

impl Sum: core::Hasher

pub fn Sum.write(mut self, bytes: Slice[u8]) limits pure, notrap, terminates {
    for byte in bytes {
        self.total = self.total.wrapping_add(u64.widen(byte))
    }
    self.writes = self.writes.wrapping_add(1)
} Sum.write

pub closed struct Pair derives Copy, Eq, Hash, Default {
    pub zeta: u8
    pub alpha: u16
} Pair

pub closed enum Shape derives Copy, Eq, Hash {
    Dot
    Line(u8)
} Shape

pub fn summed[T: Hash](value: T) -> u64 {
    var sum = Sum(total: 0, writes: 0)
    value.hash(mut sum)
    return sum.total
} summed

pub fn writes[T: Hash](value: T) -> u32 {
    var sum = Sum(total: 0, writes: 0)
    value.hash(mut sum)
    return sum.writes
} writes

pub fn probe() -> bool
    example summed(258u16) == 3
    example writes(258u16) == 1
    example summed(Pair(zeta: 1, alpha: 2)) == 3
    example writes(Pair(zeta: 1, alpha: 2)) == 2
    example summed(Shape.Line(5)) == 401u64
    example summed(core::TotalF64(value: -0.0)) == 128
    example Pair.default() == Pair(zeta: 0, alpha: 0)
{
    return true
} probe
"#;

#[test]
fn core_and_derived_hashes_write_their_bytes_through_the_hasher() {
    let scratch = Scratch::new(&[("app/package.nan", MANIFEST), ("app/app.nan", CASES)]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let found: Vec<String> = reported(&workspace, &Scope::Workspace)
        .iter()
        .map(|diagnostic| format!("{diagnostic:?}"))
        .collect();
    assert!(found.is_empty(), "{}", found.join("\n"));
}
