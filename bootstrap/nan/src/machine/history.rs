//! The history `.nan/history.nan`: a package's store name, its history head and the tombstones
//! publishing moved out of its source, by module.

use super::{Cursor, Invalid, Kind, escaped, read, write};
use crate::hash::Hash;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct History {
    pub name: String,
    /// The code root of the release last published from the package, once one is.
    pub head: Option<Hash>,
    /// Each module's tombstones, as their canonical format writes them, in byte order of module
    /// and then of text.
    pub modules: Vec<(String, Vec<String>)>,
}

impl History {
    pub fn bytes(&self) -> Option<Vec<u8>> {
        let mut lines = vec![format!("name {}", escaped(&self.name))];
        lines.extend(
            self.head
                .map(|head| format!("head {}", escaped(&head.full()))),
        );
        for (module, tombstones) in self.modules.iter().filter(|(_, held)| !held.is_empty()) {
            lines.push(format!("module {module}"));
            lines.extend(tombstones.iter().cloned());
        }
        write(Kind::History, &lines)
    }

    pub fn read(bytes: &[u8]) -> Result<History, Invalid> {
        let parsed = read(Kind::History, bytes)?;
        let mut records = parsed.records.iter().peekable();
        let mut text = |word: &str| -> Result<Option<(String, u32)>, Invalid> {
            let Some(record) =
                records.next_if(|record| record.text.starts_with(&format!("{word} ")))
            else {
                return Ok(None);
            };
            let mut cursor = Cursor::new(record);
            let value = (cursor.takes(word))
                .then(|| cursor.text())
                .flatten()
                .filter(|_| cursor.done() && record.line == record.last_line)
                .ok_or(Invalid::at(record.line))?;
            Ok(Some((value.to_owned(), record.line)))
        };
        let (name, _) = text("name")?.ok_or(Invalid::at(2))?;
        let head = match text("head")? {
            Some((head, line)) => Some(super::hash_of(&head).ok_or(Invalid::at(line))?),
            None => None,
        };
        let mut history = History {
            name,
            head,
            modules: Vec::new(),
        };
        for record in records {
            let mut cursor = Cursor::new(record);
            if cursor.takes("module") {
                let module = cursor.word().ok_or(Invalid::at(record.line))?;
                let after = history
                    .modules
                    .last()
                    .is_none_or(|(last, held)| last.as_str() < module && !held.is_empty());
                if !cursor.done() || !after {
                    return Err(Invalid::at(record.line));
                }
                history.modules.push((module.to_owned(), Vec::new()));
                continue;
            }
            let Some((_, held)) = history.modules.last_mut() else {
                return Err(Invalid::at(record.line));
            };
            let ordered = held.last().is_none_or(|last| *last < record.text);
            if !cursor.takes("retired") || !ordered {
                return Err(Invalid::at(record.line));
            }
            held.push(record.text.clone());
        }
        if history
            .modules
            .last()
            .is_some_and(|(_, held)| held.is_empty())
        {
            let last = parsed.records.last().map_or(1, |record| record.line);
            return Err(Invalid::at(last));
        }
        parsed.checked(history)
    }
}
