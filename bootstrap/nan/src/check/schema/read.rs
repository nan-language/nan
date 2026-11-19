//! Reading a schema tree from the `mem::wire` encoding of its `mem::wire::Schema`.

use std::collections::BTreeMap;

use super::{Field, Node, Variant};
use crate::artefact::read_uvar;

impl Node {
    /// The tree a `mem::wire::Schema` encoding holds: none when the bytes are not one, or when
    /// a node after the first is not the child of exactly one node before it.
    pub fn read(bytes: &[u8]) -> Option<Node> {
        let mut reader = Reader(bytes);
        let fields = reader.record()?;
        if !reader.0.is_empty() || fields.len() != 1 {
            return None;
        }
        let mut nodes = Reader(fields.get("nodes")?);
        let count = nodes.count()?;
        let flat: Vec<Flat> = (0..count)
            .map(|_| {
                let (name, payload) = nodes.variant()?;
                Flat::read(name, payload)
            })
            .collect::<Option<_>>()?;
        if !nodes.0.is_empty() {
            return None;
        }
        let mut seen = vec![false; flat.len()];
        let tree = build(&flat, 0, &mut seen)?;
        seen.iter().all(|seen| *seen).then_some(tree)
    }
}

/// A node as the encoding lists it, naming its children by position.
enum Flat {
    Leaf(Node),
    Sequence {
        element: usize,
        length: Option<u64>,
        capacity: Option<u64>,
        unique: bool,
    },
    Map {
        key: usize,
        value: usize,
    },
    Record {
        open: bool,
        fields: Vec<(String, Vec<String>, bool, usize)>,
    },
    Variant {
        open: bool,
        variants: Vec<(String, Vec<String>, Option<usize>)>,
    },
}

impl Flat {
    /// The node of a `mem::wire::SchemaNode` variant's key and payload.
    fn read(name: &str, payload: Option<&[u8]>) -> Option<Flat> {
        let fields = match payload {
            Some(payload) => Reader(payload).whole_record()?,
            None => BTreeMap::new(),
        };
        let unsigned = |key: &str| Reader(fields.get(key)?).whole(Reader::unsigned);
        let flag = |key: &str| Reader(fields.get(key)?).whole(Reader::flag);
        let known = |keys: &[&str]| fields.keys().all(|key| keys.contains(key));
        let flat = match name {
            "Bool" if payload.is_none() => Flat::Leaf(Node::Bool),
            "Char" if payload.is_none() => Flat::Leaf(Node::Char),
            "Text" if payload.is_none() => Flat::Leaf(Node::Text),
            "Bytes" if payload.is_none() => Flat::Leaf(Node::Bytes),
            "Unsigned" if known(&["width"]) => Flat::Leaf(Node::Unsigned(unsigned("width")?)),
            "Signed" if known(&["width"]) => Flat::Leaf(Node::Signed(unsigned("width")?)),
            "Float" if known(&["width"]) => Flat::Leaf(Node::Float(unsigned("width")?)),
            "BoundedText" if known(&["capacity"]) => {
                Flat::Leaf(Node::BoundedText(unsigned("capacity")?))
            }
            "Back" if known(&["depth"]) => Flat::Leaf(Node::Back(unsigned("depth")?)),
            "Sequence" if known(&["capacity", "element", "length", "unique"]) => Flat::Sequence {
                element: index(unsigned("element")?)?,
                length: Reader(fields.get("length")?).whole(Reader::option)?,
                capacity: Reader(fields.get("capacity")?).whole(Reader::option)?,
                unique: flag("unique")?,
            },
            "Map" if known(&["key", "value"]) => Flat::Map {
                key: index(unsigned("key")?)?,
                value: index(unsigned("value")?)?,
            },
            "Record" if known(&["fields", "open"]) => Flat::Record {
                open: flag("open")?,
                fields: Reader(fields.get("fields")?).whole(|reader| {
                    reader.each(|bytes| {
                        let field = Reader(bytes).whole_record()?;
                        let get = |key: &str| field.get(key).map(|bytes| Reader(bytes));
                        (field.len() == 4).then_some(())?;
                        Some((
                            get("key")?.whole(Reader::text)?,
                            get("aliases")?.whole(Reader::texts)?,
                            get("defaulted")?.whole(Reader::flag)?,
                            index(get("value")?.whole(Reader::unsigned)?)?,
                        ))
                    })
                })?,
            },
            "Variant" if known(&["open", "variants"]) => Flat::Variant {
                open: flag("open")?,
                variants: Reader(fields.get("variants")?).whole(|reader| {
                    reader.each(|bytes| {
                        let variant = Reader(bytes).whole_record()?;
                        let get = |key: &str| variant.get(key).map(|bytes| Reader(bytes));
                        (variant.len() == 3).then_some(())?;
                        let payload = get("payload")?.whole(Reader::option)?;
                        Some((
                            get("key")?.whole(Reader::text)?,
                            get("aliases")?.whole(Reader::texts)?,
                            match payload {
                                None => None,
                                Some(payload) => Some(index(payload)?),
                            },
                        ))
                    })
                })?,
            },
            _ => return None,
        };
        Some(flat)
    }
}

fn index(value: u64) -> Option<usize> {
    usize::try_from(value).ok()
}

/// The tree under the node at `at`, each child after its parent and reached once.
fn build(flat: &[Flat], at: usize, seen: &mut [bool]) -> Option<Node> {
    if std::mem::replace(seen.get_mut(at)?, true) {
        return None;
    }
    let mut child = |child: usize| (child > at).then(|| build(flat, child, seen)).flatten();
    Some(match &flat[at] {
        Flat::Leaf(node) => node.clone(),
        Flat::Sequence {
            element,
            length,
            capacity,
            unique,
        } => Node::Sequence {
            element: Box::new(child(*element)?),
            length: *length,
            capacity: *capacity,
            unique: *unique,
        },
        Flat::Map { key, value } => Node::Map {
            key: Box::new(child(*key)?),
            value: Box::new(child(*value)?),
        },
        Flat::Record { open, fields } => Node::Record {
            open: *open,
            fields: fields
                .iter()
                .map(|(key, aliases, defaulted, value)| {
                    Some(Field {
                        key: key.clone(),
                        aliases: aliases.clone(),
                        defaulted: *defaulted,
                        value: child(*value)?,
                    })
                })
                .collect::<Option<_>>()?,
            name: None,
        },
        Flat::Variant { open, variants } => Node::Variant {
            open: *open,
            variants: variants
                .iter()
                .map(|(key, aliases, payload)| {
                    Some(Variant {
                        key: key.clone(),
                        aliases: aliases.clone(),
                        payload: match payload {
                            None => None,
                            Some(payload) => Some(child(*payload)?),
                        },
                    })
                })
                .collect::<Option<_>>()?,
            name: None,
        },
    })
}

/// The bytes left to read.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    /// A value `read` reads from all the bytes.
    fn whole<T>(mut self, read: impl FnOnce(&mut Self) -> Option<T>) -> Option<T> {
        read(&mut self).filter(|_| self.0.is_empty())
    }

    fn unsigned(&mut self) -> Option<u64> {
        let (value, rest) = read_uvar(self.0)?;
        self.0 = rest;
        u64::try_from(value).ok()
    }

    fn count(&mut self) -> Option<usize> {
        self.unsigned().and_then(index)
    }

    /// The next `count` bytes, `count` being a uvar before them.
    fn counted(&mut self) -> Option<&'a [u8]> {
        let length = self.count()?;
        let taken = self.0.get(..length)?;
        self.0 = &self.0[length..];
        Some(taken)
    }

    fn flag(&mut self) -> Option<bool> {
        let (first, rest) = self.0.split_first()?;
        self.0 = rest;
        match first {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        }
    }

    fn text(&mut self) -> Option<String> {
        String::from_utf8(self.counted()?.to_vec()).ok()
    }

    fn texts(&mut self) -> Option<Vec<String>> {
        (0..self.count()?).map(|_| self.text()).collect()
    }

    /// The elements of a sequence whose elements are each read from their own bytes: a
    /// sequence of records, each a field count and its fields with their lengths.
    fn each<T>(&mut self, mut read: impl FnMut(&[u8]) -> Option<T>) -> Option<Vec<T>> {
        (0..self.count()?)
            .map(|_| {
                let start = self.0;
                for _ in 0..self.count()? {
                    self.counted()?;
                    self.counted()?;
                }
                read(&start[..start.len() - self.0.len()])
            })
            .collect()
    }

    /// A variant's key and payload.
    fn variant(&mut self) -> Option<(&'a str, Option<&'a [u8]>)> {
        let key = std::str::from_utf8(self.counted()?).ok()?;
        let payload = match self.flag()? {
            false => None,
            true => Some(self.counted()?),
        };
        Some((key, payload))
    }

    fn option(&mut self) -> Option<Option<u64>> {
        match self.variant()? {
            ("None", None) => Some(None),
            ("Some", Some(payload)) => Reader(payload).whole(Reader::unsigned).map(Some),
            _ => None,
        }
    }

    /// A record's fields by key, keys in strictly ascending byte order.
    fn record(&mut self) -> Option<BTreeMap<&'a str, &'a [u8]>> {
        let mut fields = BTreeMap::new();
        let mut last: Option<&str> = None;
        for _ in 0..self.count()? {
            let key = std::str::from_utf8(self.counted()?).ok()?;
            if last.is_some_and(|last| last >= key) {
                return None;
            }
            last = Some(key);
            fields.insert(key, self.counted()?);
        }
        Some(fields)
    }

    fn whole_record(self) -> Option<BTreeMap<&'a str, &'a [u8]>> {
        self.whole(Reader::record)
    }
}
