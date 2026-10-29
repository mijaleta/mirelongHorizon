//! JSON values: representation, parsing, printing and jq's total ordering.

use std::cmp::Ordering;
use std::rc::Rc;

/// Objects keep their keys in insertion order, like jq.
pub type Map = Vec<(String, Value)>;

#[derive(Clone, Debug)]
pub enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(Rc<str>),
    Array(Rc<Vec<Value>>),
    Object(Rc<Map>),
}

impl Value {
    pub fn string(s: &str) -> Value {
        Value::String(Rc::from(s))
    }

    pub fn array(items: Vec<Value>) -> Value {
        Value::Array(Rc::new(items))
    }

    pub fn object(map: Map) -> Value {
        Value::Object(Rc::new(map))
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    pub fn truthy(&self) -> bool {
        !matches!(self, Value::Null | Value::Bool(false))
    }

    fn rank(&self) -> u8 {
        match self {
            Value::Null => 0,
            Value::Bool(false) => 1,
            Value::Bool(true) => 2,
            Value::Number(_) => 3,
            Value::String(_) => 4,
            Value::Array(_) => 5,
            Value::Object(_) => 6,
        }
    }
}

pub fn obj_get<'a>(m: &'a Map, key: &str) -> Option<&'a Value> {
    m.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// Replaces the value of an existing key in place, or appends a new key.
pub fn obj_set(m: &mut Map, key: &str, v: Value) {
    match m.iter_mut().find(|(k, _)| k == key) {
        Some(slot) => slot.1 = v,
        None => m.push((key.to_string(), v)),
    }
}

pub fn sorted_keys(m: &Map) -> Vec<&str> {
    let mut keys: Vec<&str> = m.iter().map(|(k, _)| k.as_str()).collect();
    keys.sort();
    keys
}

/// jq's total order: null < false < true < numbers < strings < arrays < objects.
pub fn compare(a: &Value, b: &Value) -> Ordering {
    let (ra, rb) = (a.rank(), b.rank());
    if ra != rb {
        return ra.cmp(&rb);
    }
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::String(x), Value::String(y)) => x.as_bytes().cmp(y.as_bytes()),
        (Value::Array(x), Value::Array(y)) => {
            for (p, q) in x.iter().zip(y.iter()) {
                let c = compare(p, q);
                if c != Ordering::Equal {
                    return c;
                }
            }
            x.len().cmp(&y.len())
        }
        (Value::Object(x), Value::Object(y)) => {
            let (kx, ky) = (sorted_keys(x), sorted_keys(y));
            let c = kx.cmp(&ky);
            if c != Ordering::Equal {
                return c;
            }
            for k in kx {
                let c = compare(obj_get(x, k).unwrap(), obj_get(y, k).unwrap());
                if c != Ordering::Equal {
                    return c;
                }
            }
            Ordering::Equal
        }
        _ => Ordering::Equal,
    }
}

pub fn equal(a: &Value, b: &Value) -> bool {
    compare(a, b) == Ordering::Equal
}

// ---------------------------------------------------------------------------
// Parsing

pub struct JsonReader<'a> {
    s: &'a [u8],
    pos: usize,
}

impl<'a> JsonReader<'a> {
    pub fn new(s: &'a str) -> Self {
        JsonReader { s: s.as_bytes(), pos: 0 }
    }

    /// Returns the next whitespace-separated value, or None at end of input.
    pub fn next_value(&mut self) -> Option<Result<Value, String>> {
        self.skip_ws();
        if self.pos >= self.s.len() {
            return None;
        }
        Some(self.value(0))
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn fail<T>(&self, msg: &str) -> Result<T, String> {
        Err(format!("{} at byte offset {}", msg, self.pos))
    }

    fn value(&mut self, depth: usize) -> Result<Value, String> {
        if depth > 10000 {
            return self.fail("Exceeds depth limit for parsing");
        }
        self.skip_ws();
        match self.peek() {
            None => self.fail("Unfinished JSON term"),
            Some(b'{') => {
                self.pos += 1;
                let mut m = Map::new();
                self.skip_ws();
                if self.peek() == Some(b'}') {
                    self.pos += 1;
                    return Ok(Value::object(m));
                }
                loop {
                    self.skip_ws();
                    if self.peek() != Some(b'"') {
                        return self.fail("Object keys must be strings");
                    }
                    self.pos += 1;
                    let k = self.string()?;
                    self.skip_ws();
                    if self.peek() != Some(b':') {
                        return self.fail("Objects must consist of key:value pairs");
                    }
                    self.pos += 1;
                    let v = self.value(depth + 1)?;
                    obj_set(&mut m, &k, v);
                    self.skip_ws();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b'}') => {
                            self.pos += 1;
                            return Ok(Value::object(m));
                        }
                        _ => return self.fail("Expected separator between values"),
                    }
                }
            }
            Some(b'[') => {
                self.pos += 1;
                let mut items = Vec::new();
                self.skip_ws();
                if self.peek() == Some(b']') {
                    self.pos += 1;
                    return Ok(Value::array(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.skip_ws();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b']') => {
                            self.pos += 1;
                            return Ok(Value::array(items));
                        }
                        _ => return self.fail("Expected separator between values"),
                    }
                }
            }
            Some(b'"') => {
                self.pos += 1;
                Ok(Value::string(&self.string()?))
            }
            Some(b't') => self.literal("true", Value::Bool(true)),
            Some(b'f') => self.literal("false", Value::Bool(false)),
            Some(b'n') => self.literal("null", Value::Null),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            Some(_) => self.fail("Invalid literal"),
        }
    }

    fn literal(&mut self, word: &str, v: Value) -> Result<Value, String> {
        if self.s[self.pos..].starts_with(word.as_bytes()) {
            self.pos += word.len();
            if matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric()) {
                return self.fail("Invalid literal");
            }
            Ok(v)
        } else {
            self.fail("Invalid literal")
        }
    }

    fn number(&mut self) -> Result<Value, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(c) if c.is_ascii_digit() => {
                while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                    self.pos += 1;
                }
            }
            _ => return self.fail("Invalid numeric literal"),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                return self.fail("Invalid numeric literal");
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                return self.fail("Invalid numeric literal");
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'.') {
            return self.fail("Invalid numeric literal");
        }
        let text = std::str::from_utf8(&self.s[start..self.pos]).unwrap();
        text.parse::<f64>()
            .map(Value::Number)
            .or_else(|_| self.fail("Invalid numeric literal"))
    }

    /// Parses a string body; the opening quote has already been consumed.
    fn string(&mut self) -> Result<String, String> {
        let mut buf: Vec<u8> = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return self.fail("Unfinished string");
            };
            self.pos += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let Some(e) = self.peek() else {
                        return self.fail("Unfinished string");
                    };
                    self.pos += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => self.unicode_escape()?,
                        _ => return self.fail("Invalid escape"),
                    };
                    let mut tmp = [0u8; 4];
                    buf.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
                }
                _ => buf.push(c),
            }
        }
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }

    fn hex4(&mut self) -> Result<u32, String> {
        if self.pos + 4 > self.s.len() {
            return self.fail("Invalid \\uXXXX escape");
        }
        let text = std::str::from_utf8(&self.s[self.pos..self.pos + 4]).map_err(|e| e.to_string())?;
        let n = u32::from_str_radix(text, 16).or_else(|_| self.fail("Invalid \\uXXXX escape"))?;
        self.pos += 4;
        Ok(n)
    }

    fn unicode_escape(&mut self) -> Result<char, String> {
        let hi = self.hex4()?;
        if (0xD800..0xDC00).contains(&hi) && self.s[self.pos..].starts_with(b"\\u") {
            let save = self.pos;
            self.pos += 2;
            let lo = self.hex4()?;
            if (0xDC00..0xE000).contains(&lo) {
                let cp = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                return Ok(char::from_u32(cp).unwrap_or('\u{FFFD}'));
            }
            self.pos = save;
        }
        Ok(char::from_u32(hi).unwrap_or('\u{FFFD}'))
    }
}

/// Parses text that must contain exactly one JSON value.
pub fn parse_single(text: &str) -> Result<Value, String> {
    let mut r = JsonReader::new(text);
    let v = match r.next_value() {
        Some(v) => v?,
        None => return Err("Expected JSON value".to_string()),
    };
    if r.next_value().is_some() {
        return Err("Unexpected extra JSON values".to_string());
    }
    Ok(v)
}

// ---------------------------------------------------------------------------
// Printing

pub fn fmt_number(f: f64) -> String {
    if f.is_nan() {
        return "null".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "1.7976931348623157e+308" } else { "-1.7976931348623157e+308" }.to_string();
    }
    // Shortest round-trip digits, laid out the way jq prints them: exponent
    // notation when the decimal point is far left of or right of the digits.
    let sci = format!("{:e}", f);
    let (mantissa, exp) = sci.split_once('e').unwrap();
    let exp: i32 = exp.parse().unwrap();
    let negative = mantissa.starts_with('-');
    let digits: String = mantissa.chars().filter(|c| c.is_ascii_digit()).collect();
    let n = digits.len() as i32;
    let decpt = exp + 1;
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if decpt <= -4 || decpt > n + 15 {
        out.push_str(&digits[..1]);
        if n > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", exp.abs()));
    } else if decpt <= 0 {
        out.push_str("0.");
        for _ in 0..(-decpt) {
            out.push('0');
        }
        out.push_str(&digits);
    } else if decpt >= n {
        out.push_str(&digits);
        for _ in 0..(decpt - n) {
            out.push('0');
        }
    } else {
        out.push_str(&digits[..decpt as usize]);
        out.push('.');
        out.push_str(&digits[decpt as usize..]);
    }
    out
}

fn write_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn newline_indent(out: &mut String, level: usize) {
    out.push('\n');
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn write_value(v: &Value, pretty: bool, level: usize, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&fmt_number(*n)),
        Value::String(s) => write_string(s, out),
        Value::Array(a) => {
            if a.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                if pretty {
                    newline_indent(out, level + 1);
                }
                write_value(x, pretty, level + 1, out);
            }
            if pretty {
                newline_indent(out, level);
            }
            out.push(']');
        }
        Value::Object(m) => {
            if m.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                if pretty {
                    newline_indent(out, level + 1);
                }
                write_string(k, out);
                out.push(':');
                if pretty {
                    out.push(' ');
                }
                write_value(x, pretty, level + 1, out);
            }
            if pretty {
                newline_indent(out, level);
            }
            out.push('}');
        }
    }
}

pub fn to_json(v: &Value, pretty: bool) -> String {
    let mut out = String::new();
    write_value(v, pretty, 0, &mut out);
    out
}
