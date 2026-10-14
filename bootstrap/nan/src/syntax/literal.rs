//! The values literals denote.

/// The value of a `TEXT` token, its escapes replaced. The lexer has checked every escape.
pub fn text_value(token: &str) -> String {
    let mut value = String::new();
    let mut chars = token[1..token.len() - 1].chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            value.push(c);
            continue;
        }
        let escaped = match chars
            .next()
            .expect("an escape has a character after its backslash")
        {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            '0' => '\0',
            'x' => {
                let digits: String = chars.by_ref().take(2).collect();
                char::from(u8::from_str_radix(&digits, 16).expect("two hexadecimal digits"))
            }
            'u' => {
                let digits: String = chars.by_ref().skip(1).take_while(|c| *c != '}').collect();
                char::from_u32(u32::from_str_radix(&digits, 16).expect("hexadecimal digits"))
                    .expect("a scalar value")
            }
            other => other,
        };
        value.push(escaped);
    }
    value
}

#[cfg(test)]
mod tests;
