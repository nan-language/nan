//! `nan key new <principal>`, `nan key show <principal>` and `nan key trust <principal> <key>`:
//! the secret keys of the user directory and the principals file of a package.

use std::fs::File;
use std::io::{self, Read as _};
use std::path::Path;

use super::{Invocation, Trace, unlocated};
use crate::cache::{Cache, Exposed};
use crate::diagnostic::{Class, Code, Diagnostic};
use crate::journal;
use crate::machine::principals::{Principals, is_principal};
use crate::output::{Output, Record, Verdict};
use crate::sign;
use crate::workspace::Workspace;

/// `nan key new <principal>`: writes a principal's secret key from a random seed and prints its
/// public key.
pub fn new(invocation: &Invocation) -> io::Result<Output> {
    let written = invocation.args.join(" ");
    let [principal] = invocation.operands[..] else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    if !is_principal(principal) {
        return Ok(refused(
            invocation,
            &written,
            unlocated(Code::SealBadPrincipal, Vec::new()),
        ));
    }
    let cache = match Cache::open()? {
        Ok(cache) => cache,
        Err(exposed) => return Ok(refused(invocation, &written, exposed_record(&exposed.0))),
    };
    let mut seed = [0; 32];
    File::open("/dev/urandom")?.read_exact(&mut seed)?;
    let (made, path) = match cache.new_key(principal, &seed)? {
        Ok(made) => made,
        Err(exposed) => return Ok(refused(invocation, &written, exposed_record(&exposed.0))),
    };
    if !made {
        return Ok(refused(
            invocation,
            &written,
            unlocated(Code::KeyExists, Vec::new()),
        ));
    }
    let records = vec![
        key_record(principal, &sign::public_key(&seed)),
        Record {
            word: "wrote".to_owned(),
            fields: vec![path],
            lines: Vec::new(),
        },
    ];
    Ok(output(invocation, &written, records, 1))
}

/// `nan key show <principal>`: prints the public key of a principal's secret key.
pub fn show(invocation: &Invocation) -> io::Result<Output> {
    let written = invocation.args.join(" ");
    let [principal] = invocation.operands[..] else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    if !is_principal(principal) {
        return Ok(refused(
            invocation,
            &written,
            unlocated(Code::SealBadPrincipal, Vec::new()),
        ));
    }
    match secret_key(principal)? {
        Ok(Some(seed)) => {
            let records = vec![key_record(principal, &sign::public_key(&seed))];
            Ok(output(invocation, &written, records, 0))
        }
        Ok(None) => Ok(refused(
            invocation,
            &written,
            unlocated(Code::KeyMissing, Vec::new()),
        )),
        Err(exposed) => Ok(refused(invocation, &written, exposed_record(&exposed.0))),
    }
}

/// `nan key trust <principal> <key>`: gives the principal the public key in the principals file
/// of the current directory's package.
pub fn trust(
    invocation: &Invocation,
    workspace: &Workspace,
    current: &Path,
    trace: &mut Trace,
) -> io::Result<Output> {
    let written = invocation.args.join(" ");
    let (Some(package), [principal, key]) = (
        workspace
            .packages
            .iter()
            .find(|package| current.starts_with(workspace.root.join(&package.dir))),
        &invocation.operands[..],
    ) else {
        return Ok(invocation.tool_error(Code::ToolUsage, &written));
    };
    trace.scope = Some(written.clone());
    if !is_principal(principal) {
        return Ok(refused(
            invocation,
            &written,
            unlocated(Code::SealBadPrincipal, Vec::new()),
        ));
    }
    let Some(key) = sign::read_key(key) else {
        return Ok(refused(
            invocation,
            &written,
            unlocated(Code::KeyBadForm, Vec::new()),
        ));
    };
    let path = format!("{}/.nan/principals.nan", package.dir);
    let trusted = package.machine.principals().trusting(principal, key);
    let mut records = Vec::new();
    if workspace.bytes(&path) != trusted.bytes().as_deref() {
        journal::apply(&workspace.root, &[(path.clone(), trusted.bytes())])?;
        records.push(Record {
            word: "wrote".to_owned(),
            fields: vec![path],
            lines: Vec::new(),
        });
    }
    let files = records.len();
    Ok(output(invocation, &written, records, files))
}

/// The secret seed of a principal's key in the user directory, or the part of it that is exposed.
pub fn secret_key(principal: &str) -> io::Result<Result<Option<[u8; 32]>, Exposed>> {
    match Cache::open()? {
        Ok(cache) => cache.secret_key(principal),
        Err(exposed) => Ok(Err(exposed)),
    }
}

/// The seed of the secret key whose public key a principals file gives a principal, none when
/// it gives the principal no key, or the diagnostic of a key the user directory lacks.
pub fn signer(
    principals: &Principals,
    principal: &str,
) -> io::Result<Result<Option<[u8; 32]>, Diagnostic>> {
    let Some(key) = principals.key(principal) else {
        return Ok(Ok(None));
    };
    Ok(match secret_key(principal)? {
        Ok(Some(seed)) if sign::public_key(&seed) == *key => Ok(Some(seed)),
        Ok(_) => Err(Diagnostic::new(Code::KeyMissing)),
        Err(exposed) => Err(exposed_diagnostic(exposed)),
    })
}

pub fn exposed_diagnostic(exposed: Exposed) -> Diagnostic {
    Diagnostic {
        path: Some(exposed.0),
        ..Diagnostic::new(Code::BuildCacheExposed)
    }
}

fn exposed_record(path: &str) -> Record {
    Record {
        word: Class::Error.word().to_owned(),
        fields: vec![
            Code::BuildCacheExposed.name().to_owned(),
            path.to_owned(),
            "-".to_owned(),
        ],
        lines: Vec::new(),
    }
}

fn key_record(principal: &str, key: &[u8; 32]) -> Record {
    Record {
        word: "key".to_owned(),
        fields: vec![principal.to_owned(), sign::key_form(key)],
        lines: Vec::new(),
    }
}

/// The output of a key command that reports one error and writes nothing.
fn refused(invocation: &Invocation, written: &str, record: Record) -> Output {
    Output {
        command: invocation.command(written, true),
        elision: invocation.command(written, false),
        verdict: Verdict::Failed,
        summary: summary(invocation, 0, 1, 1),
        records: vec![record],
    }
}

fn output(invocation: &Invocation, written: &str, records: Vec<Record>, files: usize) -> Output {
    let count = records.len();
    Output {
        command: invocation.command(written, true),
        elision: invocation.command(written, false),
        verdict: Verdict::Ok,
        summary: summary(invocation, files, 0, count),
        records,
    }
}

/// The counts of a key command's summary.
fn summary(
    invocation: &Invocation,
    files: usize,
    errors: usize,
    records: usize,
) -> Vec<(&'static str, usize)> {
    invocation
        .counts()
        .iter()
        .map(|count| {
            let value = match *count {
                "files" => files,
                "errors" => errors,
                "records" => records,
                _ => 0,
            };
            (*count, value)
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests;
