//! The std packages this implementation checks against: the declarations of the specification's
//! `nan-api` blocks, the items it states in prose or tables, written in `declarations.nan` (which
//! restates an item of the blocks with the contract the prose gives it), and the item families it
//! declares once for one type and states for others.

include!(concat!(env!("OUT_DIR"), "/std_api.rs"));

use crate::workspace::Package;

/// The std packages in layer order.
pub const PACKAGES: [&str; 9] = [
    "core", "mem", "text", "task", "sys", "hw", "boot", "host", "testing",
];

const SUPPLEMENT: &str = include_str!("stdlib/declarations.nan");

/// A declaration family: the items owned by `source`, or named in `members` when that is not
/// empty, stated again for each of `targets` by replacing the type's name, and `width` by the
/// target's width when given.
struct Family {
    module: &'static str,
    source: &'static str,
    members: &'static [&'static str],
    targets: &'static [&'static str],
    width: Option<&'static str>,
    /// Another name the restatement replaces alongside the type's: a wrapper's scalar.
    paired: Option<(&'static str, &'static [&'static str])>,
}

const UNSIGNED: [&str; 4] = ["u8", "u16", "u64", "usize"];
const SIGNED: [&str; 4] = ["i8", "i16", "i64", "isize"];

const FAMILIES: [Family; 6] = [
    Family {
        module: "core",
        source: "u32",
        members: &[],
        targets: &UNSIGNED,
        width: None,
        paired: None,
    },
    Family {
        module: "core",
        source: "i32",
        members: &[],
        targets: &SIGNED,
        width: None,
        paired: None,
    },
    Family {
        module: "core",
        source: "u64",
        members: &["count_ones", "leading_zeros", "trailing_zeros"],
        targets: &[
            "u8", "u16", "u32", "usize", "i8", "i16", "i32", "i64", "isize",
        ],
        width: Some("64"),
        paired: None,
    },
    Family {
        module: "core",
        source: "f64",
        members: &[],
        targets: &["f32"],
        width: None,
        paired: None,
    },
    Family {
        module: "core",
        source: "TotalF64",
        members: &[],
        targets: &["TotalF32"],
        width: None,
        paired: Some(("f64", &["f32"])),
    },
    Family {
        module: "core::atomic",
        source: "AtomicU32",
        members: &[],
        targets: &["AtomicU8", "AtomicU16", "AtomicU64", "AtomicUsize"],
        width: None,
        paired: Some(("u32", &["u8", "u16", "u64", "usize"])),
    },
];

/// The text of each std module, keyed by its path: the package name, then `::` and the module's
/// name below the root.
pub fn modules() -> Vec<(String, String)> {
    let mut modules: Vec<(String, String)> = DECLARATIONS
        .iter()
        .map(|(module, text)| ((*module).to_owned(), (*text).to_owned()))
        .collect();
    let mut supplements: Vec<(usize, String)> = Vec::new();
    for line in SUPPLEMENT.lines() {
        if let Some(module) = line.strip_prefix("// module: ") {
            let index = match modules.iter().position(|(name, _)| name == module) {
                Some(index) => index,
                None => {
                    modules.push((module.to_owned(), String::new()));
                    modules.len() - 1
                }
            };
            supplements.push((index, String::new()));
            continue;
        }
        let (_, text) = supplements
            .last_mut()
            .expect("the supplement starts with a module line");
        text.push_str(line);
        text.push('\n');
    }
    for (index, supplement) in supplements {
        for chunk in chunks(&supplement) {
            let text = &mut modules[index].1;
            let head = chunk.lines().next().unwrap_or_default();
            let restated = chunks(text)
                .into_iter()
                .find(|known| !head.is_empty() && known.lines().next() == Some(head));
            match restated {
                Some(known) => *text = text.replacen(&known, &chunk, 1),
                None => text.push_str(&chunk),
            }
        }
    }
    for family in &FAMILIES {
        let (_, text) = modules
            .iter_mut()
            .find(|(name, _)| name == family.module)
            .expect("a family's module");
        let restated: String = chunks(text)
            .into_iter()
            .filter(|chunk| family.covers(chunk))
            .flat_map(|chunk| family.restate(&chunk))
            .collect();
        text.push('\n');
        text.push_str(&restated);
    }
    modules
}

/// The std packages, each with one file per module, its root module's first.
pub fn packages() -> Vec<Package> {
    let modules = modules();
    PACKAGES
        .iter()
        .map(|package| {
            let mut sources: Vec<(String, Vec<u8>)> = modules
                .iter()
                .filter(|(path, _)| path == package || path.starts_with(&format!("{package}::")))
                .map(|(path, text)| {
                    let file = match path.split_once("::") {
                        Some((_, module)) => format!("{package}/{module}/{module}.nan"),
                        None => format!("{package}/{package}.nan"),
                    };
                    let source = format!("//! The std module {path}.\n\n{text}");
                    (file, source.into_bytes())
                })
                .collect();
            sources.sort_by(|left, right| left.0.cmp(&right.0));
            Package::declared(package, sources)
        })
        .collect()
}

/// The items of a declaration text, each its lines from its column-0 head to before the next
/// item's.
fn chunks(text: &str) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    for line in text.lines() {
        let starts = !line.is_empty() && !line.starts_with(' ') && !line.starts_with('}');
        if starts || chunks.is_empty() {
            chunks.push(String::new());
        }
        let chunk = chunks.last_mut().expect("a chunk");
        chunk.push_str(line);
        chunk.push('\n');
    }
    chunks
}

impl Family {
    /// Whether a declaration belongs to the family: an item named after its source type, or one
    /// of its listed members.
    fn covers(&self, chunk: &str) -> bool {
        let head = chunk.lines().next().unwrap_or_default();
        let words: Vec<&str> = head
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '.'))
            .filter(|word| !word.is_empty())
            .collect();
        let named = |word: &str| match word.split_once('.') {
            Some((owner, member)) => {
                owner == self.source && (self.members.is_empty() || self.members.contains(&member))
            }
            None => word == self.source && self.members.is_empty(),
        };
        match words
            .iter()
            .position(|word| matches!(*word, "fn" | "struct" | "enum" | "impl" | "interface"))
        {
            Some(kind) => words.get(kind + 1).is_some_and(|word| named(word)),
            None => false,
        }
    }

    fn restate(&self, chunk: &str) -> Vec<String> {
        self.targets
            .iter()
            .enumerate()
            .map(|(index, target)| {
                let mut text = replace_word(chunk, self.source, target);
                if let Some((scalar, scalars)) = self.paired {
                    text = replace_word(&text, scalar, scalars[index]);
                }
                if let Some(width) = self.width {
                    text = replace_word(&text, width, &bits(target).to_string());
                }
                text + "\n"
            })
            .collect()
    }
}

/// The width in bits of an integer type.
fn bits(scalar: &str) -> u32 {
    match scalar {
        "u8" | "i8" => 8,
        "u16" | "i16" => 16,
        "u32" | "i32" => 32,
        _ => 64,
    }
}

/// `text` with each occurrence of `word` replaced by `with`, where no letter or `_` precedes it
/// and no letter, digit or `_` follows it: a type name stands alone or ends a literal.
fn replace_word(text: &str, word: &str, with: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(word) {
        let before = rest[..at].chars().next_back();
        let after = rest[at + word.len()..].chars().next();
        let alone = !before.is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
        out.push_str(&rest[..at]);
        out.push_str(if alone { with } else { word });
        rest = &rest[at + word.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests;
