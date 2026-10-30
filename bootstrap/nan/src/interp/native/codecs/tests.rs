use crate::check::reported;
use crate::scope::Scope;
use crate::workspace::Workspace;
use crate::workspace::tests::Scratch;

const MANIFEST: &str = "//! Codec cases.\n\npackage app {\n    language 1\n    targets x86_64-host\n    allow validate in app\n} app\n";

const CASES: &str = r#"//! Codec cases.

pub closed struct Probe derives Eq, Encode {
    pub seq: u32
    pub hops: u8
} Probe

retired Probe.ttl => Probe.hops preserves

pub open struct Config derives Eq, Encode {
    pub retries: u8
    pub verbose: bool
} Config

pub closed struct Lookup derives Encode {
    pub id: u64
    pub path: Untrusted[text::String]
} Lookup

pub closed enum Signal derives Eq, Render, Encode {
    Idle
    Level(u16)
    Pair(left: u8, right: i8)
} Signal

pub fn wire_probe(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Result[Probe, core::DecodeError]
    example wire_probe(Untrusted.of(b"\x02\x04hops\x01\x03\x03seq\x01\x05"), alloc: testing::world().alloc) == Ok(Probe(seq: 5, hops: 3))
    example wire_probe(Untrusted.of(b"\x02\x03seq\x01\x05\x03ttl\x01\x03"), alloc: testing::world().alloc) == Ok(Probe(seq: 5, hops: 3))
    example wire_probe(Untrusted.of(b"\x01\x03seq\x01\x05"), alloc: testing::world().alloc) == Err(core::DecodeError.MissingField)
    example wire_probe(Untrusted.of(b"\x02\x03seq\x01\x05\x04hops\x01\x03"), alloc: testing::world().alloc) == Err(core::DecodeError.Malformed)
    example wire_probe(Untrusted.of(b"\x02\x04hops\x01\x03\x04mode\x01\x07"), alloc: testing::world().alloc) == Err(core::DecodeError.UnknownKey)
    example wire_probe(Untrusted.of(b"\x02\x04hops\x01\x03\x03seq\x01"), alloc: testing::world().alloc) == Err(core::DecodeError.Truncated)
{
    return attempt mem::wire::Codec[Probe].decode(raw, alloc)
} wire_probe

pub fn wire_config(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Result[Config, core::DecodeError]
    example wire_config(Untrusted.of(b"\x02\x04mode\x01\x07\x07retries\x01\x03"), alloc: testing::world().alloc) == Ok(Config(retries: 3, verbose: false))
{
    return attempt mem::wire::Codec[Config].decode(raw, alloc)
} wire_config

pub fn wire_signal(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Result[Signal, core::DecodeError]
    example wire_signal(Untrusted.of(b"\x04Idle\x00"), alloc: testing::world().alloc) == Ok(Signal.Idle)
    example wire_signal(Untrusted.of(b"\x05Level\x01\x02\xAC\x02"), alloc: testing::world().alloc) == Ok(Signal.Level(300))
    example wire_signal(Untrusted.of(b"\x04Idle\x00\x00"), alloc: testing::world().alloc) == Err(core::DecodeError.Trailing)
    example wire_signal(Untrusted.of(b"\x04Idle\x01\x00"), alloc: testing::world().alloc) == Err(core::DecodeError.Malformed)
    example wire_signal(Untrusted.of(b"\x04Busy\x00"), alloc: testing::world().alloc) == Err(core::DecodeError.UnknownVariant)
{
    return attempt mem::wire::Codec[Signal].decode(raw, alloc)
} wire_signal

pub fn wire_bytes(value: Signal, alloc: mem::Alloc) -> mem::Bytes
    example wire_bytes(Signal.Pair(left: 1, right: -1), alloc: testing::world().alloc).view() == b"\x04Pair\x01\x10\x02\x04left\x01\x01\x05right\x01\x01"
    example wire_bytes(Signal.Level(300), alloc: testing::world().alloc).view() == b"\x05Level\x01\x02\xAC\x02"
{
    var out = mem::Bytes.empty(alloc)
    mem::wire::Codec[Signal].encode(value, mut out)
    return out
} wire_bytes

pub fn wire_lookup(path: Str, alloc: mem::Alloc) -> mem::Bytes
    example wire_lookup("a/b", alloc: testing::world().alloc).view() == b"\x02\x02id\x01\x07\x04path\x04\x03a/b"
{
    var out = mem::Bytes.empty(alloc)
    mem::wire::Codec[Lookup].encode(Lookup(id: 7, path: Untrusted.of(text::String.of(path, alloc))), mut out)
    return out
} wire_lookup

pub validator fn json_lookup_path(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> text::String fails core::DecodeError
    example (attempt json_lookup_path(Untrusted.of(b"{\"path\":\"a/b\",\"id\":7}"), alloc: testing::world().alloc)) == Ok(text::String.of("a/b", alloc: testing::world().alloc))
    example (attempt json_lookup_path(Untrusted.of(b"{\"path\":7,\"id\":7}"), alloc: testing::world().alloc)) == Err(core::DecodeError.Malformed)
{
    let lookup = try text::json::Codec[Lookup].decode(raw, alloc)
    return text::String.of(lookup.path.inspect().as_str(), alloc)
} json_lookup_path

pub fn json_text(value: Probe, alloc: mem::Alloc) -> text::String
    example json_text(Probe(seq: 5, hops: 3), alloc: testing::world().alloc).as_str() == "{\"hops\":3,\"seq\":5}"
{
    var out = text::String.empty(alloc)
    text::json::Codec[Probe].encode(value, mut out)
    return out
} json_text

pub fn json_floats(value: f64, alloc: mem::Alloc) -> text::String
    example json_floats(0.1, alloc: testing::world().alloc).as_str() == "0.1"
    example json_floats(100.0, alloc: testing::world().alloc).as_str() == "100.0"
    example json_floats(-0.0, alloc: testing::world().alloc).as_str() == "-0.0"
    example json_floats(0.00000015, alloc: testing::world().alloc).as_str() == "1.5e-7"
    example json_floats(20_000_000_000_000_000.0, alloc: testing::world().alloc).as_str() == "2.0e16"
    example json_floats(0.00001, alloc: testing::world().alloc).as_str() == "0.00001"
{
    var out = text::String.empty(alloc)
    text::json::Codec[f64].encode(value, mut out)
    return out
} json_floats

pub fn json_probe(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Result[Probe, core::DecodeError]
    example json_probe(Untrusted.of(b"{ \"seq\": 5, \"hops\": 3 }"), alloc: testing::world().alloc) == Ok(Probe(seq: 5, hops: 3))
    example json_probe(Untrusted.of(b"{\"seq\":5,\"hops\":3,\"seq\":6}"), alloc: testing::world().alloc) == Err(core::DecodeError.DuplicateKey)
    example json_probe(Untrusted.of(b"{\"seq\":5}"), alloc: testing::world().alloc) == Err(core::DecodeError.MissingField)
    example json_probe(Untrusted.of(b"{\"seq\":5,\"hops\":300}"), alloc: testing::world().alloc) == Err(core::DecodeError.OutOfRange)
    example json_probe(Untrusted.of(b"{\"seq\":5,\"hops\":3.5}"), alloc: testing::world().alloc) == Err(core::DecodeError.Malformed)
    example json_probe(Untrusted.of(b"{\"seq\":5,\"hops\":3} x"), alloc: testing::world().alloc) == Err(core::DecodeError.Trailing)
{
    return attempt text::json::Codec[Probe].decode(raw, alloc)
} json_probe

pub fn json_signal(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Result[Signal, core::DecodeError]
    example json_signal(Untrusted.of(b"\"Idle\""), alloc: testing::world().alloc) == Ok(Signal.Idle)
    example json_signal(Untrusted.of(b"{\"Level\": 7}"), alloc: testing::world().alloc) == Ok(Signal.Level(7))
    example json_signal(Untrusted.of(b"{\"Pair\":{\"right\":-2,\"left\":1}}"), alloc: testing::world().alloc) == Ok(Signal.Pair(left: 1, right: -2))
{
    return attempt text::json::Codec[Signal].decode(raw, alloc)
} json_signal

pub fn json_map(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Result[mem::Map[u8, u16], core::DecodeError]
    example (json_map(Untrusted.of(b"[[1,2],[3,4]]"), alloc: testing::world().alloc)).is_ok()
    example json_map(Untrusted.of(b"[[3,4],[1,2]]"), alloc: testing::world().alloc) == Err(core::DecodeError.Malformed)
    example json_map(Untrusted.of(b"[[1,2],[1,4]]"), alloc: testing::world().alloc) == Err(core::DecodeError.DuplicateKey)
{
    return attempt text::json::Codec[mem::Map[u8, u16]].decode(raw, alloc)
} json_map

pub closed struct Port derives Eq, Render, Encode {
    pub name: text::String
    pub tag: char
    pub raw: mem::Bytes
    pub signal: Signal
    pub level: i32
    pub unit: Unit
} Port

pub fn construction_port(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Result[Port, core::DecodeError]
    example construction_port(Untrusted.of(b"Port(name: \"a\\t\\u{E9}\", tag: '\\'', raw: b\"\\xFF\", signal: Signal.Pair(left: 1, right: -2), level: -1_5, unit: Unit)"), alloc: testing::world().alloc).is_ok()
    example construction_port(Untrusted.of(b"Port ( unit: Unit, level: 0, signal: Level( 7 ), raw: b\"\", tag: 'x', name: \"\" )"), alloc: testing::world().alloc).is_ok()
    example construction_port(Untrusted.of(b"Port(name: \"a\", tag: 'x', raw: b\"\", signal: Signal.Idle, level: 1, unit: Unit"), alloc: testing::world().alloc) == Err(core::DecodeError.Truncated)
    example construction_port(Untrusted.of(b"Port(name: \"\\u{D800}\", tag: 'x', raw: b\"\", signal: Signal.Idle, level: 1, unit: Unit)"), alloc: testing::world().alloc) == Err(core::DecodeError.InvalidText)
    example construction_port(Untrusted.of(b"Port(name: \"\t\", tag: 'x', raw: b\"\", signal: Signal.Idle, level: 1, unit: Unit)"), alloc: testing::world().alloc) == Err(core::DecodeError.Malformed)
    example construction_port(Untrusted.of(b"Port(name: \"a\", tag: 'xy', raw: b\"\", signal: Signal.Idle, level: 1, unit: Unit)"), alloc: testing::world().alloc) == Err(core::DecodeError.Malformed)
    example construction_port(Untrusted.of(b"Port(name: \"a\", tag: 'x', raw: b\"\", signal: Signal.Level, level: 1, unit: Unit)"), alloc: testing::world().alloc) == Err(core::DecodeError.Malformed)
    example construction_port(Untrusted.of(b"Port(name: \"a\", tag: 'x', raw: b\"\", signal: Signal.Idle, level: 1.5u8, unit: Unit)"), alloc: testing::world().alloc) == Err(core::DecodeError.Malformed)
    example construction_port(Untrusted.of(b"Port(name: <untrusted>, tag: 'x', raw: b\"\", signal: Signal.Idle, level: 1, unit: Unit)"), alloc: testing::world().alloc) == Err(core::DecodeError.Malformed)
{
    return attempt text::construction::Codec[Port].decode(raw, alloc)
} construction_port

pub fn construction_round_trip(port: Port, alloc: mem::Alloc) -> bool
    example construction_round_trip(Port(name: text::String.of("\"\u{200B}\n", alloc: testing::world().alloc), tag: '\'', raw: mem::Bytes.empty(testing::world().alloc), signal: Signal.Pair(left: 3, right: -4), level: -7, unit: Unit), alloc: testing::world().alloc)
{
    var out = text::String.empty(alloc)
    mut out.write(text::fmt("{port}", port), budget: 256)
    return attempt text::construction::Codec[Port].decode(Untrusted.of(out.as_bytes()), alloc) == Ok(port)
} construction_round_trip

pub fn construction_float(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> f64
    example construction_float(Untrusted.of(b"-1.5e-7"), alloc: testing::world().alloc) == -0.00000015
    example construction_float(Untrusted.of(b" 2.0e16"), alloc: testing::world().alloc) == 20_000_000_000_000_000.0
    example construction_float(Untrusted.of(b"1_000"), alloc: testing::world().alloc) == 1_000.0
    example construction_float(Untrusted.of(b"-inf"), alloc: testing::world().alloc) == -1.0 / 0.0
    example construction_float(Untrusted.of(b"- inf"), alloc: testing::world().alloc).is_nan()
    example construction_float(Untrusted.of(b"1.5E3"), alloc: testing::world().alloc).is_nan()
{
    match attempt text::construction::Codec[f64].decode(raw, alloc) {
        Ok(value) => return value
        Err(_) => return 0.0 / 0.0
    }
} construction_float

pub fn construction_config(raw: Untrusted[Slice[u8]], alloc: mem::Alloc) -> Result[Config, core::DecodeError]
    example construction_config(Untrusted.of(b"Config(mode: Mode.Fast([{1: \"a\"}], deep: b\"\", n: -inf), retries: 3)"), alloc: testing::world().alloc) == Ok(Config(retries: 3, verbose: false))
    example construction_config(Untrusted.of(b"Config(retries: 1_0, verbose: true) x"), alloc: testing::world().alloc) == Err(core::DecodeError.Trailing)
    example construction_config(Untrusted.of(b"Config(retries: 1__0)"), alloc: testing::world().alloc) == Err(core::DecodeError.Malformed)
    example construction_config(Untrusted.of(b"Config(retries: -1)"), alloc: testing::world().alloc) == Err(core::DecodeError.OutOfRange)
{
    return attempt text::construction::Codec[Config].decode(raw, alloc)
} construction_config

pub fn json_bytes(raw: Slice[u8], alloc: mem::Alloc) -> text::String
    example json_bytes(b"\x00\xFF", alloc: testing::world().alloc).as_str() == "\"AP8=\""
{
    var bytes = mem::Bytes.empty(alloc)
    mut bytes.append(raw)
    var out = text::String.empty(alloc)
    text::json::Codec[mem::Bytes].encode(bytes, mut out)
    return out
} json_bytes
"#;

#[test]
fn wire_json_and_construction_text_encode_and_decode_as_their_formats_state() {
    let scratch = Scratch::new(&[("app/package.nan", MANIFEST), ("app/app.nan", CASES)]);
    let workspace = Workspace::load(scratch.0.clone(), &mut Vec::new()).expect("a workspace");
    let found: Vec<String> = reported(&workspace, &Scope::Workspace)
        .iter()
        .map(|diagnostic| format!("{diagnostic:?}"))
        .collect();
    assert!(found.is_empty(), "{}", found.join("\n"));
}
