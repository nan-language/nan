//! The reader of construction text, the form derived `Render` writes.

use super::{Decoded, Failure, Input, MAX_DEPTH, Reader, fail};
use crate::syntax::lexer::is_rejected;

/// What the reader is inside: elements up to a closing byte, which the frame passes over
/// unless it closes a variant's payload, with whether one was read; a record named without
/// parentheses; or a variant, with whether its payload's parenthesis is open.
enum Frame {
    Elements {
        close: u8,
        owned: bool,
        started: bool,
    },
    Bare,
    Variant {
        payload: bool,
    },
}

/// The literal kinds, by their quote and what they hold.
#[derive(Clone, Copy, PartialEq)]
enum Literal {
    Char,
    Text,
    Bytes,
}

pub struct ConstructionReader {
    input: Input,
    frames: Vec<Frame>,
}

impl ConstructionReader {
    pub fn new(bytes: Vec<u8>) -> ConstructionReader {
        ConstructionReader {
            input: Input { bytes, at: 0 },
            frames: Vec::new(),
        }
    }

    fn byte_at(&self, at: usize) -> Option<u8> {
        self.input.bytes.get(at).copied()
    }

    /// Whether a byte follows after whitespace, passing over it when it does.
    fn follows(&mut self, byte: u8) -> bool {
        self.input.space();
        let found = self.byte_at(self.input.at) == Some(byte);
        if found {
            self.input.at += 1;
        }
        found
    }

    /// A `WORD` after whitespace.
    fn word(&mut self) -> Decoded<Vec<u8>> {
        let first = self.input.peek()?;
        if !(first.is_ascii_alphabetic() || first == b'_') {
            return fail("Malformed");
        }
        let start = self.input.at;
        while self
            .byte_at(self.input.at)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            self.input.at += 1;
        }
        Ok(self.input.bytes[start..self.input.at].to_vec())
    }

    /// Digits with `_` between two of them, at least one, as their digits alone.
    fn digits(&mut self, out: &mut String) -> Decoded<()> {
        if !self
            .byte_at(self.input.at)
            .is_some_and(|byte| byte.is_ascii_digit())
        {
            return fail("Malformed");
        }
        loop {
            match self.byte_at(self.input.at) {
                Some(byte) if byte.is_ascii_digit() => out.push(byte as char),
                Some(b'_')
                    if self
                        .byte_at(self.input.at + 1)
                        .is_some_and(|byte| byte.is_ascii_digit()) => {}
                _ => return Ok(()),
            }
            self.input.at += 1;
        }
    }

    /// A number's text without `_`, and whether it has a fraction or an exponent.
    fn number(&mut self) -> Decoded<(String, bool)> {
        self.input.peek()?;
        let mut text = String::new();
        if self.byte_at(self.input.at) == Some(b'-') {
            text.push('-');
            self.input.at += 1;
        }
        match self.byte_at(self.input.at) {
            Some(b'0') => {
                text.push('0');
                self.input.at += 1;
            }
            _ => self.digits(&mut text)?,
        }
        let point = self.byte_at(self.input.at) == Some(b'.');
        if point {
            text.push('.');
            self.input.at += 1;
            self.digits(&mut text)?;
        }
        let exponent = self.byte_at(self.input.at) == Some(b'e');
        if exponent {
            text.push('e');
            self.input.at += 1;
            if self.byte_at(self.input.at) == Some(b'-') {
                text.push('-');
                self.input.at += 1;
            }
            let from = self.input.at;
            while self
                .byte_at(self.input.at)
                .is_some_and(|byte| byte.is_ascii_digit())
            {
                self.input.at += 1;
            }
            if self.input.at == from {
                return fail("Malformed");
            }
            text.push_str(
                std::str::from_utf8(&self.input.bytes[from..self.input.at]).expect("digits"),
            );
        }
        Ok((text, point || exponent))
    }

    /// An integer without fraction or exponent, `-0` being 0.
    fn integer(&mut self) -> Decoded<i128> {
        let (text, fractional) = self.number()?;
        if fractional {
            return fail("Malformed");
        }
        text.parse::<i128>().or_else(|_| fail("OutOfRange"))
    }

    /// After whitespace, whether `-inf` follows, passing over its `-` when it does.
    fn negative_infinity(&mut self) -> Decoded<bool> {
        let found = self.input.peek()? == b'-' && self.byte_at(self.input.at + 1) == Some(b'i');
        if found {
            self.input.at += 1;
        }
        Ok(found)
    }

    /// Hexadecimal digits up to a byte, at least `least` and at most `most` of them.
    fn hex(&mut self, least: usize, most: usize) -> Decoded<u32> {
        let mut value = 0u32;
        for index in 0..most {
            let Some(byte) = self.byte_at(self.input.at) else {
                return fail("Truncated");
            };
            let Some(digit) = (byte as char).to_digit(16) else {
                return match index < least {
                    true => fail("Malformed"),
                    false => Ok(value),
                };
            };
            value = value << 4 | digit;
            self.input.at += 1;
        }
        Ok(value)
    }

    /// The value of a char, text or byte-string literal after whitespace.
    fn literal(&mut self, kind: Literal) -> Decoded<Vec<u8>> {
        let quote = match kind {
            Literal::Char => b'\'',
            Literal::Text => b'"',
            Literal::Bytes => {
                self.input.expect(b'b')?;
                if self.byte_at(self.input.at) != Some(b'"') {
                    return fail("Malformed");
                }
                b'"'
            }
        };
        self.input.expect(quote)?;
        let mut value = Vec::new();
        loop {
            let Some(byte) = self.byte_at(self.input.at) else {
                return fail("Truncated");
            };
            self.input.at += 1;
            if byte == quote {
                return Ok(value);
            }
            if byte != b'\\' {
                if kind == Literal::Bytes {
                    if !(0x20..=0x7E).contains(&byte) {
                        return fail("Malformed");
                    }
                    value.push(byte);
                    continue;
                }
                let width = match byte {
                    0x00..=0x7F => 1,
                    0xC2..=0xDF => 2,
                    0xE0..=0xEF => 3,
                    0xF0..=0xF4 => 4,
                    _ => return fail("InvalidText"),
                };
                let start = self.input.at - 1;
                if start + width > self.input.bytes.len() {
                    return fail("Truncated");
                }
                let Ok(text) = std::str::from_utf8(&self.input.bytes[start..start + width]) else {
                    return fail("InvalidText");
                };
                let c = text.chars().next().expect("a char");
                if (c as u32) < 0x20 || is_rejected(c) {
                    return fail("Malformed");
                }
                value.extend_from_slice(text.as_bytes());
                self.input.at = start + width;
                continue;
            }
            let Some(escape) = self.byte_at(self.input.at) else {
                return fail("Truncated");
            };
            self.input.at += 1;
            let scalar = match escape {
                b'\\' | b'"' | b'\'' => u32::from(escape),
                b'n' => 0x0A,
                b'r' => 0x0D,
                b't' => 0x09,
                b'0' => 0,
                b'x' => {
                    let value = self.hex(2, 2)?;
                    if value > 0x7F && kind != Literal::Bytes {
                        return fail("Malformed");
                    }
                    value
                }
                b'u' if kind != Literal::Bytes => {
                    if self.byte_at(self.input.at).is_none() {
                        return fail("Truncated");
                    }
                    if self.byte_at(self.input.at) != Some(b'{') {
                        return fail("Malformed");
                    }
                    self.input.at += 1;
                    let scalar = self.hex(1, 6)?;
                    match self.byte_at(self.input.at) {
                        None => return fail("Truncated"),
                        Some(b'}') => self.input.at += 1,
                        Some(_) => return fail("Malformed"),
                    }
                    scalar
                }
                _ => return fail("Malformed"),
            };
            match kind {
                Literal::Bytes => value.push(scalar as u8),
                _ => {
                    let c = char::from_u32(scalar).ok_or(Failure::Decode("InvalidText"))?;
                    value.extend_from_slice(c.to_string().as_bytes());
                }
            }
        }
    }

    /// Passes over any value, nested no deeper than decoding reads.
    fn skip_value(&mut self, depth: usize) -> Decoded<()> {
        let nest = |depth: usize| match depth + 1 > MAX_DEPTH {
            true => fail("TooDeep"),
            false => Ok(depth + 1),
        };
        match self.input.peek()? {
            b'"' => self.literal(Literal::Text).map(drop),
            b'\'' => self.read_char().map(drop),
            b'b' if self.byte_at(self.input.at + 1) == Some(b'"') => {
                self.literal(Literal::Bytes).map(drop)
            }
            b'[' | b'{' => {
                let depth = nest(depth)?;
                let map = self.input.peek()? == b'{';
                let close = if map { b'}' } else { b']' };
                self.input.at += 1;
                self.frames.push(Frame::Elements {
                    close,
                    owned: true,
                    started: false,
                });
                while self.next_follows()? {
                    self.skip_value(depth)?;
                    if map {
                        self.input.expect(b':')?;
                        self.skip_value(depth)?;
                    }
                }
                self.end()
            }
            byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                self.word()?;
                if self.follows(b'.') {
                    self.word()?;
                }
                if !self.follows(b'(') {
                    return Ok(());
                }
                let depth = nest(depth)?;
                self.frames.push(Frame::Elements {
                    close: b')',
                    owned: true,
                    started: false,
                });
                while self.next_follows()? {
                    let at = self.input.at;
                    let labelled = self.word().is_ok() && self.follows(b':');
                    if !labelled {
                        self.input.at = at;
                    }
                    self.skip_value(depth)?;
                }
                self.end()
            }
            _ if self.negative_infinity()? => match self.word()?.as_slice() {
                b"inf" => Ok(()),
                _ => fail("Malformed"),
            },
            _ => self.number().map(drop),
        }
    }
}

impl Reader for ConstructionReader {
    fn read_bool(&mut self) -> Decoded<bool> {
        match self.word()?.as_slice() {
            b"true" => Ok(true),
            b"false" => Ok(false),
            _ => fail("Malformed"),
        }
    }

    fn read_unsigned(&mut self) -> Decoded<u64> {
        u64::try_from(self.integer()?).or_else(|_| fail("OutOfRange"))
    }

    fn read_signed(&mut self) -> Decoded<i64> {
        i64::try_from(self.integer()?).or_else(|_| fail("OutOfRange"))
    }

    fn read_float(&mut self) -> Decoded<f64> {
        let negative = self.negative_infinity()?;
        let first = self.input.peek()?;
        if negative || first.is_ascii_alphabetic() || first == b'_' {
            return match (negative, self.word()?.as_slice()) {
                (false, b"NaN") => Ok(f64::NAN),
                (false, b"inf") => Ok(f64::INFINITY),
                (true, b"inf") => Ok(f64::NEG_INFINITY),
                _ => fail("Malformed"),
            };
        }
        let (text, _) = self.number()?;
        text.parse::<f64>().or_else(|_| fail("Malformed"))
    }

    fn read_char(&mut self) -> Decoded<u32> {
        let text = self.literal(Literal::Char)?;
        let text = String::from_utf8(text).expect("a literal's text");
        let mut chars = text.chars();
        match (chars.next(), chars.next()) {
            (Some(c), None) => Ok(c as u32),
            _ => fail("Malformed"),
        }
    }

    fn read_text(&mut self) -> Decoded<Vec<u8>> {
        self.literal(Literal::Text)
    }

    fn read_bytes(&mut self) -> Decoded<Vec<u8>> {
        self.literal(Literal::Bytes)
    }

    fn begin_sequence(&mut self) -> Decoded<()> {
        self.input.expect(b'[')?;
        self.frames.push(Frame::Elements {
            close: b']',
            owned: true,
            started: false,
        });
        Ok(())
    }

    fn begin_map(&mut self) -> Decoded<()> {
        self.input.expect(b'{')?;
        self.frames.push(Frame::Elements {
            close: b'}',
            owned: true,
            started: false,
        });
        Ok(())
    }

    fn begin_record(&mut self) -> Decoded<()> {
        self.word()?;
        let frame = match self.follows(b'(') {
            true => Frame::Elements {
                close: b')',
                owned: true,
                started: false,
            },
            false => Frame::Bare,
        };
        self.frames.push(frame);
        Ok(())
    }

    fn begin_labelled(&mut self) -> Decoded<()> {
        self.frames.push(Frame::Elements {
            close: b')',
            owned: false,
            started: false,
        });
        Ok(())
    }

    fn next_follows(&mut self) -> Decoded<bool> {
        let (close, started) = match self.frames.last_mut() {
            Some(Frame::Bare) => return Ok(false),
            Some(Frame::Elements { close, started, .. }) => {
                (*close, std::mem::replace(started, true))
            }
            _ => return fail("Malformed"),
        };
        if self.input.peek()? == close {
            return Ok(false);
        }
        if started {
            self.input.expect(b',')?;
        }
        Ok(true)
    }

    fn map_value(&mut self) -> Decoded<()> {
        self.input.expect(b':')
    }

    fn read_key(&mut self) -> Decoded<Vec<u8>> {
        let key = self.word()?;
        self.input.expect(b':')?;
        Ok(key)
    }

    fn read_variant(&mut self) -> Decoded<Vec<u8>> {
        let mut key = self.word()?;
        if self.follows(b'.') {
            key = self.word()?;
        }
        self.frames.push(Frame::Variant { payload: false });
        Ok(key)
    }

    fn payload_follows(&mut self) -> Decoded<bool> {
        let follows = self.follows(b'(');
        match self.frames.last_mut() {
            Some(Frame::Variant { payload }) => {
                *payload = follows;
                Ok(follows)
            }
            _ => fail("Malformed"),
        }
    }

    fn skip(&mut self, depth: usize) -> Decoded<()> {
        self.skip_value(depth)
    }

    fn end(&mut self) -> Decoded<()> {
        match self.frames.pop() {
            Some(Frame::Elements {
                close, owned: true, ..
            }) => self.input.expect(close),
            Some(Frame::Variant { payload: true }) => self.input.expect(b')'),
            Some(_) => Ok(()),
            None => fail("Malformed"),
        }
    }

    fn finish(&mut self) -> Decoded<()> {
        self.input.space();
        match self.input.at == self.input.bytes.len() {
            true => Ok(()),
            false => fail("Trailing"),
        }
    }
}
