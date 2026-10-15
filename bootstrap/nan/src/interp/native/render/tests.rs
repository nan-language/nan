use crate::check::reported;
use crate::scope::Scope;
use crate::workspace::Workspace;
use crate::workspace::tests::Scratch;

const MANIFEST: &str =
    "//! Render cases.\n\npackage app {\n    language 1\n    targets x86_64-host\n} app\n";

const CASES: &str = r#"//! Render cases.

pub closed enum Kind derives Copy, Eq, Render {
    Data
    Ack
    Sized(bytes: u16, last: bool)
} Kind

pub closed struct Header derives Copy, Eq, Render {
    pub kind: Kind
    pub seq: u32
} Header

pub closed struct Empty derives Render {
} Empty

pub closed struct Level {
    pub value: u8
} Level

impl Level: Render

pub fn Level.render(self, mut out: core::Renderer) {
    mut out.write("level ")
    mut out.write_char('#')
    if out.remaining() > 0 {
        mut out.write("!")
    }
} Level.render

pub fn shown(header: Header, alloc: mem::Alloc) -> text::String
    example shown(Header(kind: Kind.Ack, seq: 7), alloc: testing::world().alloc).as_str() == "Header(kind: Kind.Ack, seq: 7)"
    example shown(Header(kind: Kind.Sized(bytes: 3, last: true), seq: 0), alloc: testing::world().alloc).as_str() == "Header(kind: Kind.Sized(bytes: 3, last: true), seq: 0)"
{
    var out = text::String.empty(alloc)
    mut out.write(text::fmt("{header}", header), budget: 64)
    return out
} shown

pub fn clipped(line: Str, alloc: mem::Alloc) -> text::String
    example clipped("a long line", alloc: testing::world().alloc).as_str() == "a lon..."
    example clipped("short", alloc: testing::world().alloc).as_str() == "short"
    example clipped("ééééé", alloc: testing::world().alloc).as_str() == "éé..."
{
    var out = text::String.empty(alloc)
    mut out.write(text::fmt("{line}", line), budget: 8)
    return out
} clipped

pub fn described(seq: u32, total: u32, alloc: mem::Alloc) -> text::String
    example described(3, total: 9, alloc: testing::world().alloc).as_str() == "frame 3 of 9, {ok}"
{
    var out = text::String.empty(alloc)
    mut out.write(text::fmt("frame {seq} of {total}, {{ok}}", seq, total), budget: 64)
    return out
} described

pub fn values(alloc: mem::Alloc) -> text::String
    example values(testing::world().alloc).as_str() == "[1, 2] Some(-3) None 0.1 -0.0 1.5e-7 true x Empty() level #! b\"a\\n\\xFF\" <untrusted>"
{
    var out = text::String.empty(alloc)
    let pair = Array[u8, 2].filled(1)
    var list = mem::List[u8].empty(alloc)
    mut list.push(1)
    mut list.push(2)
    var bytes = mem::Bytes.empty(alloc)
    mut bytes.push(97)
    mut bytes.push(10)
    mut bytes.push(255)
    let some: Option[i8] = Some(-3)
    let none: Option[i8] = None
    let hidden = Untrusted.of(7u8)
    mut out.write(text::fmt("{list} {some} {none} {a} {b} {c} {t} {x} {e} {level} {bytes} {hidden}", list, some, none, a: 0.1f64, b: -0.0f64, c: 0.00000015f64, t: true, x: 'x', e: Empty(), level: Level(value: 1), bytes, hidden), budget: 200)
    discard pair
    return out
} values

pub fn fixed(alloc: mem::Alloc) -> text::Fixed[6]
    example fixed(testing::world().alloc).as_str() == "ab12"
{
    var out = text::Fixed[6].of("ab")
    mut out.write(text::fmt("{n}", n: 12u8), budget: 64)
    discard alloc
    return out
} fixed
"#;

#[test]
fn templates_and_derived_and_builtin_renderings_write_their_text_within_the_budget() {
    let scratch = Scratch::new(&[("app/package.nan", MANIFEST), ("app/app.nan", CASES)]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let found: Vec<String> = reported(&workspace, &Scope::Workspace)
        .iter()
        .map(|diagnostic| format!("{diagnostic:?}"))
        .collect();
    assert!(found.is_empty(), "{}", found.join("\n"));
}
