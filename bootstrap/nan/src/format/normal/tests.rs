use super::*;
use crate::syntax::file::parse;

/// The forms of the first item of `source`, its paths naming `example` items when capitalised
/// and core ones when scalar.
fn forms_of(source: &str, own: &str, facts: Option<&str>, transparent: bool) -> Forms {
    let file = parse(source.as_bytes().to_vec());
    let item = file
        .items
        .iter()
        .find(|item| item.tree.is_some())
        .expect("an item");
    let resolve = |segments: &[&str]| match segments {
        ["u8" | "u32" | "Slice"] => Some(segments[0].to_owned()),
        [name] if name.starts_with(char::is_uppercase) => Some(format!("example::{name}")),
        ["helper"] => Some("example::helper".to_owned()),
        _ => None,
    };
    forms(
        &file,
        item,
        &Known {
            resolve: &resolve,
            own: Some(own.to_owned()),
            transparent,
            fields: Reach::Consumer,
            facts: facts.map(str::to_owned),
        },
    )
}

#[test]
fn a_function_is_flat_with_canonical_paths_and_numbered_bindings() {
    let forms = forms_of(
        "/// Raises the level.\npub fn Gauge.raise(mut self, step: u32)\n    ensures self.level >= old(self.level)\n{\n    let next = self.level.saturating_add(step) // added\n    self.level = next\n} Gauge.raise\n",
        "example::Gauge.raise",
        None,
        false,
    );
    assert_eq!(
        forms.normalised,
        "pub fn example::Gauge.raise(mut self, step: u32)\n    ensures self.level >= old(self.level)\n{\n    let $1 = self.level.saturating_add(step)\n    self.level = $1\n} example::Gauge.raise\n"
    );
    assert_eq!(
        forms.interface.as_deref(),
        Some(
            "pub fn example::Gauge.raise(mut self, step: u32)\n    ensures self.level >= old(self.level)\n"
        )
    );
}

#[test]
fn literals_order_free_members_and_punned_bindings_are_canonical() {
    let forms = forms_of(
        "fn pick(values: Slice[u8], limit: u32) -> u32 {\n    let total = 0x1_0u32\n    let label = \"a\\\"b\\n\"\n    return match total {\n        2 => helper(values,\n            total)\n        1 => helper(values, limit: limit)\n    }\n} pick\n",
        "example::pick",
        None,
        false,
    );
    assert_eq!(
        forms.normalised,
        "fn example::pick(values: Slice[u8], limit: u32) -> u32 {\n    let $1 = 16u32\n    let $2 = \"a\\x22b\\x0A\"\n    return match $1 {\n        1 => example::helper(values, limit)\n        2 => example::helper(values, total: $1)\n    }\n} example::pick\n"
    );
}

#[test]
fn a_struct_shows_its_visible_fields_and_facts() {
    let forms = forms_of(
        "pub closed struct Meter derives Hash, Eq {\n    pub peak: u32\n    count: u32 where count <= 100\n    invariant count <= peak\n    pub base: u32\n} Meter\n",
        "example::Meter",
        Some("facts Copy, Send"),
        false,
    );
    assert_eq!(
        forms.normalised,
        "pub closed struct example::Meter derives Eq, Hash {\n    count: u32 where count <= 100\n    pub base: u32\n    invariant count <= peak\n    pub peak: u32\n} example::Meter\n"
    );
    assert_eq!(
        forms.interface.as_deref(),
        Some(
            "pub closed struct example::Meter derives Eq, Hash {\n    pub base: u32\n    pub peak: u32\n} example::Meter\nfacts Copy, Send\n"
        )
    );
}

#[test]
fn reduced_forms_drop_what_naming_code_does_not_see() {
    let transparent = "pub fn Meter.value(self) -> u32 = self.count\n";
    let hidden = forms_of(transparent, "example::Meter.value", None, false);
    assert_eq!(
        hidden.interface.as_deref(),
        Some("pub fn example::Meter.value(self) -> u32\n")
    );
    let shown = forms_of(transparent, "example::Meter.value", None, true);
    assert_eq!(shown.interface, Some(shown.normalised));
    let test = forms_of(
        "test bump_counts for Meter.bump {\n    expect 1 == 1\n} bump_counts\n",
        "example::bump_counts",
        None,
        false,
    );
    assert_eq!(
        test.interface.as_deref(),
        Some("test example::bump_counts for example::Meter.bump\n")
    );
    let constant = forms_of("pub const STEP: u32 = 5\n", "example::STEP", None, false);
    assert_eq!(constant.normalised, "pub const example::STEP: u32 = 5\n");
    assert_eq!(constant.interface, None);
}
