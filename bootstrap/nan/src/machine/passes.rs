//! The passes file `.nan/passes.nan`: the passes of a package's tests that principals recorded on
//! run targets, each signed when the package's principals file gives its principal a key.

use super::principals::{Principals, is_principal};
use super::{Cursor, Invalid, Kind, escaped, read, write};
use crate::hash::Hash;
use crate::sign;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pass {
    pub test_path: String,
    pub target: String,
    pub principal: String,
    pub reach: Hash,
    pub signature: Option<[u8; 64]>,
}

impl Pass {
    /// Whether the pass records a reach hash of `reach` and, when `principals` gives its principal
    /// a key, holds a signature that verifies by that key, or else holds none.
    pub fn current(&self, reach: Hash, principals: &Principals) -> bool {
        let message =
            || sign::pass_message(&self.principal, &self.test_path, &self.target, self.reach);
        self.reach == reach
            && match (principals.key(&self.principal), &self.signature) {
                (Some(key), Some(signature)) => sign::verify(key, &message(), signature),
                (None, None) => true,
                _ => false,
            }
    }

    /// The key a file orders and replaces passes by: the test, the run target and the principal.
    fn key(&self) -> (&str, &str, &str) {
        (&self.test_path, &self.target, &self.principal)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Passes {
    /// In byte order of test, then of run target, then of principal.
    pub passes: Vec<Pass>,
}

impl Passes {
    /// The file holding `pass` in place of the pass of the same key it held.
    pub fn recording(&self, pass: Pass) -> Passes {
        let mut passes: Vec<Pass> = self
            .passes
            .iter()
            .filter(|held| held.key() != pass.key())
            .cloned()
            .collect();
        let at = passes.partition_point(|held| held.key() < pass.key());
        passes.insert(at, pass);
        Passes { passes }
    }

    pub fn bytes(&self) -> Option<Vec<u8>> {
        let mut lines = Vec::new();
        for pass in &self.passes {
            lines.push(format!(
                "pass {} {} {} {}",
                escaped(&pass.test_path),
                pass.target,
                escaped(&pass.principal),
                escaped(&pass.reach.full())
            ));
            if let Some(signature) = &pass.signature {
                lines.push(format!(
                    "signature {}",
                    escaped(&sign::signature_form(signature))
                ));
            }
        }
        write(Kind::Passes, &lines)
    }

    pub fn read(bytes: &[u8]) -> Result<Passes, Invalid> {
        let parsed = read(Kind::Passes, bytes)?;
        let mut passes = Passes::default();
        for record in &parsed.records {
            let mut cursor = Cursor::new(record);
            let fail = Invalid::at(record.line);
            match cursor.word().ok_or(fail)? {
                "pass" => {
                    let test_path = cursor.text().ok_or(fail)?.to_owned();
                    let target = cursor.target().ok_or(fail)?;
                    let principal = cursor
                        .text()
                        .filter(|text| is_principal(text))
                        .ok_or(fail)?
                        .to_owned();
                    let reach = cursor.hash().ok_or(fail)?;
                    let pass = Pass {
                        test_path,
                        target,
                        principal,
                        reach,
                        signature: None,
                    };
                    let after = passes
                        .passes
                        .last()
                        .is_none_or(|last| last.key() < pass.key());
                    if !after {
                        return Err(fail);
                    }
                    passes.passes.push(pass);
                }
                "signature" => {
                    let signature = cursor.text().and_then(sign::read_signature).ok_or(fail)?;
                    let pass = passes.passes.last_mut().ok_or(fail)?;
                    if pass.signature.replace(signature).is_some() {
                        return Err(fail);
                    }
                }
                _ => return Err(fail),
            }
            if !cursor.done() {
                return Err(fail);
            }
        }
        parsed.checked(passes)
    }
}
