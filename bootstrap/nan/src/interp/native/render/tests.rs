use std::process::Command;

use super::float_text;
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

pub closed struct Named derives Render {
    pub name: text::String
    pub mark: char
    pub shared: mem::Shared[text::String]
    pub words: mem::Map[text::String, Option[char]]
} Named

pub fn nested(alloc: mem::Alloc) -> text::String
    example nested(testing::world().alloc).as_str() == "Named(name: \"a\\\"\\\\\\u{7F}\\u{200B}\u{E9}\\r\\0\", mark: '\\'', shared: \"'\", words: {\"k\": Some('\"')}) a\"\\\u{7F}"
{
    var words = mem::Map[text::String, Option[char]].empty(alloc)
    discard mut words.insert(text::String.of("k", alloc), value: Some('"'))
    let named = Named(name: text::String.of("a\"\\\u{7F}\u{200B}\u{E9}\r\0", alloc), mark: '\'', shared: mem::Shared.of(text::String.of("'", alloc), alloc), words: take words)
    var out = text::String.empty(alloc)
    mut out.write(text::fmt("{named} {text}", named, text: "a\"\\\u{7F}"), budget: 200)
    return out
} nested

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

const FLOATS_MANIFEST: &str = "//! Floats read and written.\n\npackage floats {\n    language 1\n    targets x86_64-host\n    entry main\n    allow validate in floats\n} floats\n";

const FLOATS: &str = r#"//! Decodes each argument as an `f64` and an `f32` through JSON and construction text, and logs each value with its JSON encoding.

fn logged[T: Encode and Render](log: sys::Log, label: Str, decoded: Result[T, core::DecodeError], alloc: mem::Alloc) {
    match decoded {
        Ok(value) => {
            var json = text::String.empty(alloc)
            text::json::Codec[T].encode(value, out: mut json)
            log.info(label, value: value)
            log.info(label, value: json.as_str())
        }
        Err(error) => log.info(label, value: error)
    }
} logged

validator fn decode_each(args: Untrusted[mem::List[mem::Bytes]], log: sys::Log, alloc: mem::Alloc) {
    for arg in args.inspect() {
        let raw = Untrusted.of(arg.view())
        logged(log, label: "j64", decoded: attempt text::json::Codec[f64].decode(raw, alloc), alloc)
        logged(log, label: "j32", decoded: attempt text::json::Codec[f32].decode(raw, alloc), alloc)
        logged(log, label: "c64", decoded: attempt text::construction::Codec[f64].decode(raw, alloc), alloc)
        logged(log, label: "c32", decoded: attempt text::construction::Codec[f32].decode(raw, alloc), alloc)
    }
} decode_each

fn main(take world: host::World) fails task::Cancelled uses wait {
    decode_each(world.args, log: world.log, alloc: world.alloc)
} main
"#;

/// A xorshift generator of bit patterns.
struct Patterns(u64);

impl Patterns {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn finite(&mut self) -> f64 {
        loop {
            let value = f64::from_bits(self.next());
            if value.is_finite() {
                return value;
            }
        }
    }
}

/// The exact value of a finite f64's magnitude: its decimal digits and the power of ten of the
/// last.
fn exact(value: f64) -> (Vec<u8>, i32) {
    let written = format!("{:.1100e}", value.abs());
    let (mantissa, exponent) = written.split_once('e').expect("an exponent");
    let digits = mantissa
        .bytes()
        .filter(u8::is_ascii_digit)
        .map(|d| d - b'0');
    let exponent: i32 = exponent.parse().expect("an exponent");
    (digits.collect(), exponent - 1100)
}

/// The value halfway between two positive finite f64 values: its decimal digits and the power of
/// ten of the last.
fn halfway(low: f64, high: f64) -> (String, i32) {
    let ((mut a, ea), (mut b, eb)) = (exact(low), exact(high));
    let least = ea.min(eb);
    a.extend(std::iter::repeat_n(0, (ea - least) as usize));
    b.extend(std::iter::repeat_n(0, (eb - least) as usize));
    let width = a.len().max(b.len()) + 2;
    let pad = |digits: Vec<u8>| -> Vec<u8> {
        let mut padded = vec![0; width - digits.len()];
        padded.extend(digits);
        padded
    };
    let (a, b) = (pad(a), pad(b));
    let mut half = vec![0u8; width];
    let mut carry = 0;
    for index in (0..width).rev() {
        let total = a[index] + b[index] + carry;
        half[index] = total % 10;
        carry = total / 10;
    }
    for digit in half.iter_mut().rev() {
        let total = *digit * 5 + carry;
        *digit = total % 10;
        carry = total / 10;
    }
    let first = half
        .iter()
        .position(|digit| *digit != 0)
        .unwrap_or(width - 1);
    let last = half
        .iter()
        .rposition(|digit| *digit != 0)
        .unwrap_or(width - 1);
    let digits = half[first..=last]
        .iter()
        .map(|digit| char::from(b'0' + digit))
        .collect();
    (digits, least - 1 + (width - 1 - last) as i32)
}

/// Decimal numbers that JSON and construction text both read: random floats of both widths
/// written shortest and with long exact expansions, the points halfway between neighbouring
/// floats with decimals just above and below them, and the edges of the f64 and f32 ranges.
fn decimals() -> Vec<String> {
    let mut patterns = Patterns(0x9E37_79B9_7F4A_7C15);
    let mut inputs: Vec<String> = [
        "0",
        "-0",
        "0.0",
        "0e5",
        "1",
        "0.1",
        "1e-5",
        "9.999999999999999e-6",
        "1e16",
        "9999999999999998",
        "0.000000000000000000000000000000000001",
        "1e-400",
        "-1e400",
        "1e99999999999999999999999",
        "1e-99999999999999999999999",
        "1.7976931348623157e308",
        "1.7976931348623158e308",
        "1.7976931348623159e308",
        "4.9406564584124654e-324",
        "2.4703282292062327e-324",
        "2.4703282292062328e-324",
        "2.2250738585072011e-308",
        "2.2250738585072014e-308",
        "3.4028235e38",
        "3.4028236e38",
        "1.4e-45",
        "7e-46",
        "123456789012345678901234567890",
    ]
    .map(str::to_owned)
    .into();
    inputs.push(format!("0.{}1", "0".repeat(1200)));
    inputs.push(format!("1.{}1", "0".repeat(1200)));
    inputs.push(format!("2.{}", "9".repeat(1000)));
    for _ in 0..600 {
        inputs.push(format!("{:e}", patterns.finite()));
        let single = f32::from_bits(patterns.next() as u32 & 0x7F7F_FFFF);
        inputs.push(format!("-{single:e}"));
    }
    for _ in 0..150 {
        let precision = (patterns.next() % 780) as usize;
        inputs.push(format!("{:.precision$e}", patterns.finite()));
    }
    for _ in 0..150 {
        let low = f64::from_bits(patterns.next() % 0x7FEF_FFFF_FFFF_FFFF);
        let (digits, exponent) = halfway(low, f64::from_bits(low.to_bits() + 1));
        let lower = &digits[..digits.len() - 1];
        inputs.push(format!("{digits}e{exponent}"));
        inputs.push(format!("{digits}01e{}", exponent - 2));
        inputs.push(format!("{lower}49e{}", exponent - 1));
    }
    for _ in 0..100 {
        let low = f32::from_bits(patterns.next() as u32 % 0x7F7F_FFFF);
        let high = f32::from_bits(low.to_bits() + 1);
        inputs.push(format!(
            "{:e}",
            f64::midpoint(f64::from(low), f64::from(high))
        ));
    }
    inputs
}

/// What the image logs for a decimal: its f64 and f32 read through JSON and then through
/// construction text, each rendered and then encoded as JSON.
fn logged(decimal: &str) -> String {
    let wide: f64 = decimal.parse().expect("a decimal");
    let narrow = wide as f32;
    let wide_text = float_text(wide, None);
    let narrow_text = float_text(f64::from(narrow), Some(narrow));
    let mut lines = String::new();
    for (label, text) in [
        ("j64", &wide_text),
        ("j32", &narrow_text),
        ("c64", &wide_text),
        ("c32", &narrow_text),
    ] {
        let json = match text.starts_with('N') || text.ends_with("inf") {
            true => format!("\"{text}\""),
            false => text.clone(),
        };
        lines.push_str(&format!("info: {label} {text}\ninfo: {label} {json}\n"));
    }
    lines
}

/// An image writes and reads floats as the interpreter does: it renders and encodes as JSON the
/// shortest digits that read back, and reads decimals rounded to nearest, ties to even.
#[test]
fn images_write_and_read_floats_as_the_interpreter() {
    let scratch = Scratch::new(&[
        ("floats/package.nan", FLOATS_MANIFEST),
        ("floats/floats.nan", FLOATS),
    ]);
    let built = crate::run(&["build".to_owned()], &scratch.0);
    assert_eq!(built.status, 0, "{}", built.output);
    let image = scratch.0.join(".nan/images/x86_64-host/floats");
    for batch in decimals().chunks(200) {
        let run = Command::new(&image)
            .args(batch)
            .output()
            .expect("run the image");
        let written = String::from_utf8(run.stderr).expect("UTF-8 output");
        let mut lines = written.lines();
        for decimal in batch {
            let got: String = lines
                .by_ref()
                .take(8)
                .map(|line| format!("{line}\n"))
                .collect();
            assert_eq!(got, logged(decimal), "for {decimal}");
        }
    }
}
