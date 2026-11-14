//! JSON values as the language server reads and writes them.

/// A JSON value: an object keeps its members in order, a number its text.
#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(String),
    Text(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

/// The deepest nesting of arrays and objects a message may hold.
const DEPTH: u32 = 64;

impl Json {
    /// The object of members given in order.
    pub fn object(members: Vec<(&str, Json)>) -> Json {
        Json::Object(
            members
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value))
                .collect(),
        )
    }

    pub fn text(text: &str) -> Json {
        Json::Text(text.to_owned())
    }

    pub fn number(number: u64) -> Json {
        Json::Number(number.to_string())
    }

    /// An object's member of that name.
    pub fn get(&self, name: &str) -> Option<&Json> {
        match self {
            Json::Object(members) => members
                .iter()
                .find(|(member, _)| member == name)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    /// The value the names reach, each an object's member.
    pub fn at(&self, names: &[&str]) -> Option<&Json> {
        names.iter().try_fold(self, |value, name| value.get(name))
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Text(text) => Some(text),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// A number without sign, fraction or exponent that fits in 32 bits.
    pub fn as_u32(&self) -> Option<u32> {
        match self {
            Json::Number(text) if text.bytes().all(|byte| byte.is_ascii_digit()) => {
                text.parse().ok()
            }
            _ => None,
        }
    }

    /// An array's elements, none for another value.
    pub fn elements(&self) -> &[Json] {
        match self {
            Json::Array(elements) => elements,
            _ => &[],
        }
    }

    /// The value written compactly, members in their order.
    pub fn written(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut String) {
        match self {
            Json::Null => out.push_str("null"),
            Json::Bool(true) => out.push_str("true"),
            Json::Bool(false) => out.push_str("false"),
            Json::Number(text) => out.push_str(text),
            Json::Text(text) => write_text(text, out),
            Json::Array(elements) => {
                out.push('[');
                for (index, element) in elements.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    element.write(out);
                }
                out.push(']');
            }
            Json::Object(members) => {
                out.push('{');
                for (index, (name, value)) in members.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write_text(name, out);
                    out.push(':');
                    value.write(out);
                }
                out.push('}');
            }
        }
    }
}

/// A string literal: `"` and `\` escaped, control characters by their escapes.
fn write_text(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

/// The value a message's bytes hold, with whitespace around it; none when they hold no single
/// valid value or nest deeper than `DEPTH`.
pub fn parse(bytes: &[u8]) -> Option<Json> {
    let mut reader = Reader { bytes, at: 0 };
    let value = reader.value(0)?;
    reader.skip_space();
    (reader.at == bytes.len()).then_some(value)
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn skip_space(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    /// Takes `word` when the bytes continue with it.
    fn word(&mut self, word: &[u8]) -> bool {
        let taken = self.bytes[self.at..].starts_with(word);
        if taken {
            self.at += word.len();
        }
        taken
    }

    fn value(&mut self, depth: u32) -> Option<Json> {
        self.skip_space();
        match self.peek()? {
            b'n' if self.word(b"null") => Some(Json::Null),
            b't' if self.word(b"true") => Some(Json::Bool(true)),
            b'f' if self.word(b"false") => Some(Json::Bool(false)),
            b'"' => self.text().map(Json::Text),
            b'[' if depth < DEPTH => self.array(depth + 1),
            b'{' if depth < DEPTH => self.members(depth + 1),
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }

    fn array(&mut self, depth: u32) -> Option<Json> {
        self.at += 1;
        let mut elements = Vec::new();
        self.skip_space();
        if self.word(b"]") {
            return Some(Json::Array(elements));
        }
        loop {
            elements.push(self.value(depth)?);
            self.skip_space();
            if self.word(b"]") {
                return Some(Json::Array(elements));
            }
            if !self.word(b",") {
                return None;
            }
        }
    }

    fn members(&mut self, depth: u32) -> Option<Json> {
        self.at += 1;
        let mut members = Vec::new();
        self.skip_space();
        if self.word(b"}") {
            return Some(Json::Object(members));
        }
        loop {
            self.skip_space();
            if self.peek()? != b'"' {
                return None;
            }
            let name = self.text()?;
            self.skip_space();
            if !self.word(b":") {
                return None;
            }
            members.push((name, self.value(depth)?));
            self.skip_space();
            if self.word(b"}") {
                return Some(Json::Object(members));
            }
            if !self.word(b",") {
                return None;
            }
        }
    }

    fn number(&mut self) -> Option<Json> {
        let start = self.at;
        self.word(b"-");
        match self.peek()? {
            b'0' => self.at += 1,
            b'1'..=b'9' => self.digits(),
            _ => return None,
        }
        if self.word(b".") {
            self.required_digits()?;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            self.required_digits()?;
        }
        let text = std::str::from_utf8(&self.bytes[start..self.at]).ok()?;
        Some(Json::Number(text.to_owned()))
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.at += 1;
        }
    }

    fn required_digits(&mut self) -> Option<()> {
        let start = self.at;
        self.digits();
        (self.at > start).then_some(())
    }

    /// A string literal, its escapes decoded; a lone surrogate decodes as U+FFFD.
    fn text(&mut self) -> Option<String> {
        self.at += 1;
        let mut text = String::new();
        loop {
            let start = self.at;
            while !matches!(self.peek()?, b'"' | b'\\' | 0..0x20) {
                self.at += 1;
            }
            text.push_str(std::str::from_utf8(&self.bytes[start..self.at]).ok()?);
            match self.peek()? {
                b'"' => {
                    self.at += 1;
                    return Some(text);
                }
                b'\\' => {
                    self.at += 1;
                    text.push(self.escape()?);
                }
                _ => return None,
            }
        }
    }

    fn escape(&mut self) -> Option<char> {
        let byte = self.peek()?;
        self.at += 1;
        Some(match byte {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let unit = self.unit(self.at)?;
                self.at += 4;
                if !(0xd800..0xdc00).contains(&unit) {
                    return Some(char::from_u32(unit).unwrap_or('\u{fffd}'));
                }
                let low = self.bytes[self.at..]
                    .starts_with(b"\\u")
                    .then(|| self.unit(self.at + 2))
                    .flatten()
                    .filter(|low| (0xdc00..0xe000).contains(low));
                let Some(low) = low else {
                    return Some('\u{fffd}');
                };
                self.at += 6;
                char::from_u32(0x10000 + ((unit - 0xd800) << 10) + (low - 0xdc00))?
            }
            _ => return None,
        })
    }

    /// The four hexadecimal digits at `at`.
    fn unit(&self, at: usize) -> Option<u32> {
        let digits = self.bytes.get(at..at + 4)?;
        if !digits.iter().all(u8::is_ascii_hexdigit) {
            return None;
        }
        u32::from_str_radix(std::str::from_utf8(digits).ok()?, 16).ok()
    }
}
