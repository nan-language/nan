//! The principals file `.nan/principals.nan`: the public key each principal of a package signs
//! its seals with.

use super::{Cursor, Invalid, Kind, escaped, read, write};
use crate::sign;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Principals {
    /// In byte order of principal.
    pub keys: Vec<(String, [u8; 32])>,
}

/// Whether a text is a principal: 1 to 64 characters, each an ASCII letter, digit, `.`, `_`, `-`
/// or `@`.
pub fn is_principal(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-@".contains(&byte))
}

impl Principals {
    /// The public key the file gives a principal.
    pub fn key(&self, principal: &str) -> Option<&[u8; 32]> {
        self.keys
            .iter()
            .find(|(named, _)| named == principal)
            .map(|(_, key)| key)
    }

    /// The file giving a principal a key, replacing the key it gave it.
    pub fn trusting(&self, principal: &str, key: [u8; 32]) -> Principals {
        let mut keys: Vec<(String, [u8; 32])> = self
            .keys
            .iter()
            .filter(|(named, _)| named != principal)
            .cloned()
            .collect();
        let at = keys.partition_point(|(named, _)| named.as_str() < principal);
        keys.insert(at, (principal.to_owned(), key));
        Principals { keys }
    }

    pub fn bytes(&self) -> Option<Vec<u8>> {
        let lines: Vec<String> = self
            .keys
            .iter()
            .map(|(principal, key)| {
                format!(
                    "principal {} {}",
                    escaped(principal),
                    escaped(&sign::key_form(key))
                )
            })
            .collect();
        write(Kind::Principals, &lines)
    }

    pub fn read(bytes: &[u8]) -> Result<Principals, Invalid> {
        let parsed = read(Kind::Principals, bytes)?;
        let mut principals = Principals::default();
        for record in &parsed.records {
            let mut cursor = Cursor::new(record);
            let fail = Invalid::at(record.line);
            if !cursor.takes("principal") {
                return Err(fail);
            }
            let principal = cursor
                .text()
                .filter(|text| is_principal(text))
                .ok_or(fail)?;
            let key = cursor.text().and_then(sign::read_key).ok_or(fail)?;
            let after = principals
                .keys
                .last()
                .is_none_or(|(last, _)| last.as_str() < principal);
            if !cursor.done() || !after {
                return Err(fail);
            }
            principals.keys.push((principal.to_owned(), key));
        }
        parsed.checked(principals)
    }
}
