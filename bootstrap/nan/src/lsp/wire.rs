//! The base protocol's framing: each message a header block, ended by an empty line, whose
//! `Content-Length` gives the byte length of the JSON content after it.

use std::io::{self, BufRead, Write};

/// The largest content a message may hold.
const LARGEST: usize = 1 << 26;

/// The content of the next message, none at the end of the input or after a header block giving
/// no valid `Content-Length`.
pub fn read(input: &mut dyn BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut length = None;
    let mut line = Vec::new();
    loop {
        line.clear();
        if input.read_until(b'\n', &mut line)? == 0 {
            return Ok(None);
        }
        let header = line
            .strip_suffix(b"\n")
            .map(|rest| rest.strip_suffix(b"\r").unwrap_or(rest))
            .unwrap_or(&line);
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_at(header.len().min(15));
        if name.eq_ignore_ascii_case(b"content-length:") {
            length = std::str::from_utf8(value)
                .ok()
                .and_then(|value| value.trim().parse::<usize>().ok());
        }
    }
    let Some(length) = length.filter(|length| *length <= LARGEST) else {
        return Ok(None);
    };
    let mut content = vec![0; length];
    match input.read_exact(&mut content) {
        Ok(()) => Ok(Some(content)),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Err(error) => Err(error),
    }
}

/// Writes a message holding `content`.
pub fn write(output: &mut dyn Write, content: &str) -> io::Result<()> {
    write!(output, "Content-Length: {}\r\n\r\n{content}", content.len())?;
    output.flush()
}
