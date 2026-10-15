//! Place paths: a root local and the fields after it, and when two overlap.

use crate::check::tir::LocalId;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Path {
    pub root: LocalId,
    pub fields: Vec<String>,
}

impl Path {
    pub fn of(root: LocalId) -> Path {
        Path {
            root,
            fields: Vec::new(),
        }
    }

    pub fn field(&self, name: &str) -> Path {
        let mut fields = self.fields.clone();
        fields.push(name.to_owned());
        Path {
            root: self.root,
            fields,
        }
    }

    /// Whether this path is `prefix` or extends it.
    pub fn starts_with(&self, prefix: &Path) -> bool {
        self.root == prefix.root && self.fields.starts_with(&prefix.fields)
    }

    /// Whether one of two paths extends the other.
    pub fn overlaps(&self, other: &Path) -> bool {
        self.starts_with(other) || other.starts_with(self)
    }

    /// The path as source writes it, its root named `root`.
    pub fn written(&self, root: &str) -> String {
        std::iter::once(root)
            .chain(self.fields.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(".")
    }
}
