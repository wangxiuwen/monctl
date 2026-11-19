// Tiny JSON: parse and write the values monctl's bundled data files (phone
// models) and the phone module's state files (restore records, zoom memory)
// use. Documents are small, so the whole thing is parsed into memory; no
// streaming, no schema. Kept dependency-free like the rest of monctl.

#[derive(Clone, Debug, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    /// First member named `key`, or None.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(members) => members.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_num(&self) -> Option<f64> {
        match self {
            Json::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Json::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&[Json]> {
        match self {
            Json::Arr(items) => Some(items),
            _ => None,
        }
    }

    /// The object's members, in order.
    pub fn members(&self) -> Option<&[(String, Json)]> {
        match self {
            Json::Obj(members) => Some(members),
            _ => None,
        }
    }

    pub fn write(&self) -> String {
        match self {
            Json::Null => "null".into(),
            Json::Bool(b) => b.to_string(),
            Json::Num(n) => {
                if n.fract() == 0.0 && n.abs() < 9e15 {
                    format!("{}", *n as i64)
                } else {
                    format!("{}", n)
                }
            }
            Json::Str(s) => write_string(s),
            Json::Arr(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.write()).collect();
                format!("[{}]", parts.join(","))
            }
            Json::Obj(members) => {
                let parts: Vec<String> = members.iter().map(|(k, v)| format!("{}:{}", write_string(k), v.write())).collect();
                format!("{{{}}}", parts.join(","))
            }
        }
    }
}

fn write_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Parse one JSON document; leading and trailing whitespace is fine.
pub fn parse(s: &str) -> Result<Json, String> {
    let b: Vec<char> = s.chars().collect();
    let mut pos = 0usize;
    let v = value(&b, &mut pos)?;
    ws(&b, &mut pos);
    if pos != b.len() {
        return Err(format!("trailing data at byte {}", pos));
    }
    Ok(v)
}

fn ws(b: &[char], pos: &mut usize) {
    while *pos < b.len() && b[*pos].is_whitespace() {
        *pos += 1;
    }
}

fn peek(b: &[char], pos: usize) -> Option<char> {
    b.get(pos).copied()
}

fn value(b: &[char], pos: &mut usize) -> Result<Json, String> {
    ws(b, pos);
    match peek(b, *pos) {
        Some('{') => object(b, pos),
        Some('[') => array(b, pos),
        Some('"') => Ok(Json::Str(string_lit(b, pos)?)),
        Some('t') => literal(b, pos, "true", Json::Bool(true)),
        Some('f') => literal(b, pos, "false", Json::Bool(false)),
        Some('n') => literal(b, pos, "null", Json::Null),
        Some(c) if c == '-' || c.is_ascii_digit() => number(b, pos),
        Some(c) => Err(format!("unexpected '{}' at byte {}", c, pos)),
        None => Err("unexpected end of input".into()),
    }
}

fn literal(b: &[char], pos: &mut usize, word: &str, v: Json) -> Result<Json, String> {
    for c in word.chars() {
        if peek(b, *pos) != Some(c) {
            return Err(format!("bad literal at byte {}", pos));
        }
        *pos += 1;
    }
    Ok(v)
}

fn object(b: &[char], pos: &mut usize) -> Result<Json, String> {
    *pos += 1; // '{'
    let mut members = Vec::new();
    ws(b, pos);
    if peek(b, *pos) == Some('}') {
        *pos += 1;
        return Ok(Json::Obj(members));
    }
    loop {
        ws(b, pos);
        if peek(b, *pos) != Some('"') {
            return Err(format!("expected member name at byte {}", pos));
        }
        let key = string_lit(b, pos)?;
        ws(b, pos);
        if peek(b, *pos) != Some(':') {
            return Err(format!("expected ':' at byte {}", pos));
        }
        *pos += 1;
        let v = value(b, pos)?;
        members.push((key, v));
        ws(b, pos);
        match peek(b, *pos) {
            Some(',') => *pos += 1,
            Some('}') => {
                *pos += 1;
                return Ok(Json::Obj(members));
            }
            _ => return Err(format!("expected ',' or '}}' at byte {}", pos)),
        }
    }
}

fn array(b: &[char], pos: &mut usize) -> Result<Json, String> {
    *pos += 1; // '['
    let mut items = Vec::new();
    ws(b, pos);
    if peek(b, *pos) == Some(']') {
        *pos += 1;
        return Ok(Json::Arr(items));
    }
    loop {
        items.push(value(b, pos)?);
        ws(b, pos);
        match peek(b, *pos) {
            Some(',') => *pos += 1,
            Some(']') => {
                *pos += 1;
                return Ok(Json::Arr(items));
            }
            _ => return Err(format!("expected ',' or ']' at byte {}", pos)),
        }
    }
}

fn string_lit(b: &[char], pos: &mut usize) -> Result<String, String> {
    *pos += 1; // '"'
    let mut out = String::new();
    loop {
        let c = peek(b, *pos).ok_or("unterminated string")?;
        *pos += 1;
        match c {
            '"' => return Ok(out),
            '\\' => {
                let e = peek(b, *pos).ok_or("unterminated escape")?;
                *pos += 1;
                match e {
                    '"' => out.push('"'),
                    '\\' => out.push('\\'),
                    '/' => out.push('/'),
                    'b' => out.push('\u{0008}'),
                    'f' => out.push('\u{000c}'),
                    'n' => out.push('\n'),
                    'r' => out.push('\r'),
                    't' => out.push('\t'),
                    'u' => {
                        let hi = hex4(b, pos)?;
                        let cp = if (0xD800..0xDC00).contains(&hi) {
                            // surrogate pair: \uD83D\uDE00 and friends
                            if peek(b, *pos) != Some('\\') || peek(b, *pos + 1) != Some('u') {
                                return Err("lone leading surrogate".into());
                            }
                            *pos += 2;
                            let lo = hex4(b, pos)?;
                            if !(0xDC00..0xE000).contains(&lo) {
                                return Err("bad trailing surrogate".into());
                            }
                            0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                        } else if (0xDC00..0xE000).contains(&hi) {
                            return Err("lone trailing surrogate".into());
                        } else {
                            hi
                        };
                        out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                    }
                    _ => return Err(format!("bad escape '\\{}'", e)),
                }
            }
            c if (c as u32) < 0x20 => return Err("control character in string".into()),
            c => out.push(c),
        }
    }
}

fn hex4(b: &[char], pos: &mut usize) -> Result<u32, String> {
    let hex: String = b.get(*pos..*pos + 4).ok_or("truncated \\u escape")?.iter().collect();
    *pos += 4;
    u32::from_str_radix(&hex, 16).map_err(|_| format!("bad \\u escape '{}'", hex))
}

fn number(b: &[char], pos: &mut usize) -> Result<Json, String> {
    let start = *pos;
    if peek(b, *pos) == Some('-') {
        *pos += 1;
    }
    while matches!(peek(b, *pos), Some(c) if c.is_ascii_digit()) {
        *pos += 1;
    }
    if peek(b, *pos) == Some('.') {
        *pos += 1;
        while matches!(peek(b, *pos), Some(c) if c.is_ascii_digit()) {
            *pos += 1;
        }
    }
    if matches!(peek(b, *pos), Some('e') | Some('E')) {
        *pos += 1;
        if matches!(peek(b, *pos), Some('+') | Some('-')) {
            *pos += 1;
        }
        while matches!(peek(b, *pos), Some(c) if c.is_ascii_digit()) {
            *pos += 1;
        }
    }
    let text: String = b[start..*pos].iter().collect();
    text.parse::<f64>().map(Json::Num).map_err(|_| format!("bad number '{}'", text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_documents() {
        let v = parse(r#"{"brands":[{"name":"Apple","models":[{"name":"iPhone 8","screen":[750,1334],"points":375}]},{"name":"华为"}]}"#).unwrap();
        assert_eq!(v.get("brands").unwrap().as_arr().unwrap().len(), 2);
        let m = &v.get("brands").unwrap().as_arr().unwrap()[0].get("models").unwrap().as_arr().unwrap()[0];
        assert_eq!(m.get("name").unwrap().as_str(), Some("iPhone 8"));
        assert_eq!(m.get("screen").unwrap().as_arr().unwrap()[1].as_num(), Some(1334.0));
        assert_eq!(m.get("points").unwrap().as_num(), Some(375.0));
    }

    #[test]
    fn parses_escapes_and_surrogate_pairs() {
        let v = parse(r#""a\"b\\c\/d\b\f\n\r\t😀A""#).unwrap();
        assert_eq!(v.as_str(), Some("a\"b\\c/d\u{0008}\u{000c}\n\r\t😀A"));
    }

    #[test]
    fn parses_numbers_and_literals() {
        assert_eq!(parse("-12.5e2").unwrap(), Json::Num(-1250.0));
        assert_eq!(parse("0").unwrap(), Json::Num(0.0));
        assert_eq!(parse("[true,false,null]").unwrap(), Json::Arr(vec![Json::Bool(true), Json::Bool(false), Json::Null]));
    }

    #[test]
    fn write_roundtrips() {
        let src = r#"{"a":"中文\"x","n":42,"f":0.5,"b":[true,null]}"#;
        let v = parse(src).unwrap();
        let out = v.write();
        assert_eq!(parse(&out).unwrap(), v);
    }

    #[test]
    fn rejects_broken_documents() {
        assert!(parse("").is_err());
        assert!(parse("{").is_err());
        assert!(parse("{\"a\":}").is_err());
        assert!(parse("[1,]").is_err());
        assert!(parse("{} extra").is_err());
        assert!(parse("\"\\ud800\"").is_err()); // lone surrogate escape
    }
}
