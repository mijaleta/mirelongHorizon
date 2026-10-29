//! Lexer and recursive-descent parser for the jqlite filter language.

use crate::value::Value;
use std::rc::Rc;

pub type P = Rc<Ast>;

#[derive(Debug)]
pub enum Ast {
    Identity,
    Recurse,
    Literal(Value),
    Str(Vec<StrPart>),
    /// target, key; the key expression sees the same input as the target.
    Index(P, P),
    Slice(P, Option<P>, Option<P>),
    Iterate(P),
    Array(Option<P>),
    Object(Vec<(P, P)>),
    Neg(P),
    Pipe(P, P),
    Comma(P, P),
    Binop(BinOp, P, P),
    And(P, P),
    Or(P, P),
    Alt(P, P),
    Assign(AssignOp, P, P),
    If(P, P, Option<P>),
    Try(P, Option<P>),
    Reduce(P, String, P, P),
    Foreach(P, String, P, P, Option<P>),
    As(P, String, P),
    Def(Rc<FuncDef>, P),
    Call(String, Vec<P>),
    Var(String),
}

#[derive(Debug)]
pub enum StrPart {
    Lit(String),
    Expr(P),
}

#[derive(Debug)]
pub struct FuncDef {
    pub name: String,
    pub params: Vec<Param>,
    pub body: P,
}

#[derive(Debug)]
pub enum Param {
    Filter(String),
    Value(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AssignOp {
    Set,
    Update,
    Arith(BinOp),
    Alt,
}

// ---------------------------------------------------------------------------
// Lexer

#[derive(Debug, Clone, PartialEq)]
enum Lx {
    Ident(String),
    Field(String),
    Var(String),
    Num(f64),
    Str(Vec<LxStr>),
    Dot,
    DotDot,
    Pipe,
    Comma,
    LParen,
    RParen,
    LBrack,
    RBrack,
    LBrace,
    RBrace,
    Colon,
    Semi,
    Question,
    Op(&'static str),
    End,
}

#[derive(Debug, Clone, PartialEq)]
enum LxStr {
    Lit(String),
    Interp(Vec<Lx>),
}

const OPS: [&str; 20] = [
    "//=", "|=", "+=", "-=", "*=", "/=", "%=", "==", "!=", "<=", ">=", "//", "=", "<", ">", "+", "-", "*", "/", "%",
];

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

struct Lexer {
    c: Vec<char>,
    i: usize,
}

impl Lexer {
    fn at(&self, k: usize) -> Option<char> {
        self.c.get(self.i + k).copied()
    }

    fn starts_with(&self, s: &str) -> bool {
        s.chars().enumerate().all(|(k, ch)| self.at(k) == Some(ch))
    }

    fn skip_space(&mut self) {
        loop {
            match self.at(0) {
                Some(c) if c.is_whitespace() => self.i += 1,
                Some('#') => {
                    while !matches!(self.at(0), None | Some('\n')) {
                        self.i += 1;
                    }
                }
                _ => return,
            }
        }
    }

    fn ident(&mut self) -> String {
        let start = self.i;
        while self.at(0).map_or(false, is_ident_char) {
            self.i += 1;
        }
        self.c[start..self.i].iter().collect()
    }

    fn number(&mut self) -> Result<f64, String> {
        let start = self.i;
        while self.at(0).map_or(false, |c| c.is_ascii_digit()) {
            self.i += 1;
        }
        if self.at(0) == Some('.') && self.at(1).map_or(false, |c| c.is_ascii_digit()) {
            self.i += 1;
            while self.at(0).map_or(false, |c| c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        if matches!(self.at(0), Some('e' | 'E')) {
            let save = self.i;
            self.i += 1;
            if matches!(self.at(0), Some('+' | '-')) {
                self.i += 1;
            }
            if self.at(0).map_or(false, |c| c.is_ascii_digit()) {
                while self.at(0).map_or(false, |c| c.is_ascii_digit()) {
                    self.i += 1;
                }
            } else {
                self.i = save;
            }
        }
        let text: String = self.c[start..self.i].iter().collect();
        text.parse::<f64>().map_err(|_| format!("syntax error: invalid number {}", text))
    }

    /// Lexes until end of input or, when `nested`, until the `)` closing a string interpolation.
    fn lex_all(&mut self, nested: bool) -> Result<Vec<Lx>, String> {
        let mut out = Vec::new();
        let mut depth = 0usize;
        loop {
            self.skip_space();
            let Some(ch) = self.at(0) else {
                if nested {
                    return Err("syntax error: unterminated string interpolation".to_string());
                }
                out.push(Lx::End);
                return Ok(out);
            };
            let next = self.at(1);
            match ch {
                '"' => {
                    self.i += 1;
                    out.push(self.string()?);
                }
                '.' if next == Some('.') => {
                    self.i += 2;
                    out.push(Lx::DotDot);
                }
                '.' if next.map_or(false, is_ident_start) => {
                    self.i += 1;
                    out.push(Lx::Field(self.ident()));
                }
                '.' => {
                    self.i += 1;
                    out.push(Lx::Dot);
                }
                '$' => {
                    self.i += 1;
                    if !self.at(0).map_or(false, is_ident_start) {
                        return Err("syntax error: expected a variable name after '$'".to_string());
                    }
                    out.push(Lx::Var(self.ident()));
                }
                c if is_ident_start(c) => out.push(Lx::Ident(self.ident())),
                c if c.is_ascii_digit() => out.push(Lx::Num(self.number()?)),
                '(' => {
                    self.i += 1;
                    depth += 1;
                    out.push(Lx::LParen);
                }
                ')' => {
                    self.i += 1;
                    if nested && depth == 0 {
                        out.push(Lx::End);
                        return Ok(out);
                    }
                    depth = depth.saturating_sub(1);
                    out.push(Lx::RParen);
                }
                '[' | ']' | '{' | '}' | ':' | ';' | ',' | '?' => {
                    self.i += 1;
                    out.push(match ch {
                        '[' => Lx::LBrack,
                        ']' => Lx::RBrack,
                        '{' => Lx::LBrace,
                        '}' => Lx::RBrace,
                        ':' => Lx::Colon,
                        ';' => Lx::Semi,
                        ',' => Lx::Comma,
                        _ => Lx::Question,
                    });
                }
                '|' if next != Some('=') => {
                    self.i += 1;
                    out.push(Lx::Pipe);
                }
                _ => match OPS.iter().find(|op| self.starts_with(op)) {
                    Some(op) => {
                        self.i += op.len();
                        out.push(Lx::Op(op));
                    }
                    None => return Err(format!("syntax error: unexpected character '{}'", ch)),
                },
            }
        }
    }

    /// Lexes a string literal; the opening quote has already been consumed.
    fn string(&mut self) -> Result<Lx, String> {
        let mut parts = Vec::new();
        let mut buf = String::new();
        loop {
            let Some(ch) = self.at(0) else {
                return Err("syntax error: unterminated string literal".to_string());
            };
            self.i += 1;
            match ch {
                '"' => break,
                '\\' => {
                    let Some(e) = self.at(0) else {
                        return Err("syntax error: unterminated string literal".to_string());
                    };
                    self.i += 1;
                    match e {
                        '"' => buf.push('"'),
                        '\\' => buf.push('\\'),
                        '/' => buf.push('/'),
                        'b' => buf.push('\u{8}'),
                        'f' => buf.push('\u{c}'),
                        'n' => buf.push('\n'),
                        'r' => buf.push('\r'),
                        't' => buf.push('\t'),
                        'u' => buf.push(self.unicode_escape()?),
                        '(' => {
                            if !buf.is_empty() {
                                parts.push(LxStr::Lit(std::mem::take(&mut buf)));
                            }
                            parts.push(LxStr::Interp(self.lex_all(true)?));
                        }
                        other => return Err(format!("syntax error: invalid escape '\\{}'", other)),
                    }
                }
                c => buf.push(c),
            }
        }
        if !buf.is_empty() || parts.is_empty() {
            parts.push(LxStr::Lit(buf));
        }
        Ok(Lx::Str(parts))
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let text: String = (0..4).filter_map(|k| self.at(k)).collect();
        if text.len() != 4 {
            return Err("syntax error: invalid \\u escape".to_string());
        }
        let n = u32::from_str_radix(&text, 16).map_err(|_| "syntax error: invalid \\u escape".to_string())?;
        self.i += 4;
        Ok(n)
    }

    fn unicode_escape(&mut self) -> Result<char, String> {
        let hi = self.hex4()?;
        if (0xD800..0xDC00).contains(&hi) && self.starts_with("\\u") {
            let save = self.i;
            self.i += 2;
            let lo = self.hex4()?;
            if (0xDC00..0xE000).contains(&lo) {
                let cp = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                return Ok(char::from_u32(cp).unwrap_or('\u{FFFD}'));
            }
            self.i = save;
        }
        Ok(char::from_u32(hi).unwrap_or('\u{FFFD}'))
    }
}

// ---------------------------------------------------------------------------
// Parser

const KEYWORDS: [&str; 13] = [
    "def", "if", "then", "elif", "else", "end", "as", "reduce", "foreach", "try", "catch", "and", "or",
];

fn rc(a: Ast) -> P {
    Rc::new(a)
}

fn lit_str(s: &str) -> P {
    rc(Ast::Literal(Value::string(s)))
}

fn index(target: P, key: P) -> P {
    rc(Ast::Index(target, key))
}

fn describe(t: &Lx) -> String {
    match t {
        Lx::Ident(s) => format!("'{}'", s),
        Lx::Field(s) => format!("'.{}'", s),
        Lx::Var(s) => format!("'${}'", s),
        Lx::Num(n) => format!("'{}'", n),
        Lx::Str(_) => "string literal".to_string(),
        Lx::Op(o) => format!("'{}'", o),
        Lx::End => "end of input".to_string(),
        other => format!("{:?}", other),
    }
}

struct Parser {
    t: Vec<Lx>,
    i: usize,
}

impl Parser {
    fn peek(&self) -> &Lx {
        &self.t[self.i]
    }

    fn peek_at(&self, k: usize) -> &Lx {
        self.t.get(self.i + k).unwrap_or(&Lx::End)
    }

    fn bump(&mut self) -> Lx {
        let t = self.t[self.i].clone();
        if self.i + 1 < self.t.len() {
            self.i += 1;
        }
        t
    }

    fn is_kw(&self, kw: &str) -> bool {
        matches!(self.peek(), Lx::Ident(s) if s == kw)
    }

    fn unexpected<T>(&self, wanted: &str) -> Result<T, String> {
        Err(format!("syntax error: expected {} but found {}", wanted, describe(self.peek())))
    }

    fn expect(&mut self, want: Lx, what: &str) -> Result<(), String> {
        if *self.peek() == want {
            self.bump();
            Ok(())
        } else {
            self.unexpected(what)
        }
    }

    fn expect_kw(&mut self, kw: &str) -> Result<(), String> {
        if self.is_kw(kw) {
            self.bump();
            Ok(())
        } else {
            self.unexpected(&format!("'{}'", kw))
        }
    }

    fn var_name(&mut self) -> Result<String, String> {
        match self.peek().clone() {
            Lx::Var(n) => {
                self.bump();
                Ok(n)
            }
            _ => self.unexpected("a $variable"),
        }
    }

    fn pipe(&mut self) -> Result<P, String> {
        if self.is_kw("def") {
            let fd = self.funcdef()?;
            let rest = self.pipe()?;
            return Ok(rc(Ast::Def(fd, rest)));
        }
        let first = self.unary()?;
        if self.is_kw("as") {
            self.bump();
            let name = self.var_name()?;
            self.expect(Lx::Pipe, "'|'")?;
            let body = self.pipe()?;
            return Ok(rc(Ast::As(first, name, body)));
        }
        let lhs = self.binary_rest(first, 1)?;
        if *self.peek() == Lx::Pipe {
            self.bump();
            let rhs = self.pipe()?;
            return Ok(rc(Ast::Pipe(lhs, rhs)));
        }
        Ok(lhs)
    }

    /// (precedence, right-associative) of the binary operator at the cursor, if any.
    fn binop_info(&self) -> Option<(u8, bool)> {
        match self.peek() {
            Lx::Comma => Some((1, false)),
            Lx::Op("//") => Some((2, true)),
            Lx::Op("=" | "|=" | "+=" | "-=" | "*=" | "/=" | "%=" | "//=") => Some((3, false)),
            Lx::Ident(s) if s == "or" => Some((4, false)),
            Lx::Ident(s) if s == "and" => Some((5, false)),
            Lx::Op("==" | "!=" | "<" | "<=" | ">" | ">=") => Some((6, false)),
            Lx::Op("+" | "-") => Some((7, false)),
            Lx::Op("*" | "/" | "%") => Some((8, false)),
            _ => None,
        }
    }

    fn binary_rest(&mut self, mut lhs: P, min: u8) -> Result<P, String> {
        while let Some((prec, right)) = self.binop_info() {
            if prec < min {
                break;
            }
            let op = self.bump();
            let first = self.unary()?;
            let rhs = self.binary_rest(first, if right { prec } else { prec + 1 })?;
            lhs = make_binop(op, lhs, rhs);
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<P, String> {
        if *self.peek() == Lx::Op("-") {
            self.bump();
            let e = self.unary()?;
            return Ok(rc(Ast::Neg(e)));
        }
        self.postfix()
    }

    fn postfix(&mut self) -> Result<P, String> {
        let mut term = self.primary()?;
        loop {
            match self.peek().clone() {
                Lx::Field(name) => {
                    self.bump();
                    term = index(term, lit_str(&name));
                }
                Lx::Dot if matches!(self.peek_at(1), Lx::Str(_)) => {
                    self.bump();
                    let key = self.string_literal()?;
                    term = index(term, key);
                }
                Lx::Dot if *self.peek_at(1) == Lx::LBrack => {
                    self.bump();
                    term = self.bracket(term)?;
                }
                Lx::LBrack => term = self.bracket(term)?,
                Lx::Question => {
                    self.bump();
                    term = rc(Ast::Try(term, None));
                }
                _ => break,
            }
        }
        Ok(term)
    }

    /// Parses `[]`, `[e]`, `[e:e]`, `[e:]` or `[:e]` applied to `term`.
    fn bracket(&mut self, term: P) -> Result<P, String> {
        self.expect(Lx::LBrack, "'['")?;
        if *self.peek() == Lx::RBrack {
            self.bump();
            return Ok(rc(Ast::Iterate(term)));
        }
        if *self.peek() == Lx::Colon {
            self.bump();
            let to = self.pipe()?;
            self.expect(Lx::RBrack, "']'")?;
            return Ok(rc(Ast::Slice(term, None, Some(to))));
        }
        let e = self.pipe()?;
        if *self.peek() == Lx::Colon {
            self.bump();
            let to = if *self.peek() == Lx::RBrack { None } else { Some(self.pipe()?) };
            self.expect(Lx::RBrack, "']'")?;
            return Ok(rc(Ast::Slice(term, Some(e), to)));
        }
        self.expect(Lx::RBrack, "']'")?;
        Ok(index(term, e))
    }

    fn string_literal(&mut self) -> Result<P, String> {
        match self.bump() {
            Lx::Str(parts) => self.string_from(parts),
            _ => Err("syntax error: expected a string".to_string()),
        }
    }

    fn string_from(&mut self, parts: Vec<LxStr>) -> Result<P, String> {
        if parts.iter().all(|p| matches!(p, LxStr::Lit(_))) {
            let s: String = parts
                .into_iter()
                .map(|p| match p {
                    LxStr::Lit(s) => s,
                    LxStr::Interp(_) => unreachable!(),
                })
                .collect();
            return Ok(rc(Ast::Literal(Value::string(&s))));
        }
        let mut out = Vec::new();
        for p in parts {
            match p {
                LxStr::Lit(s) => out.push(StrPart::Lit(s)),
                LxStr::Interp(toks) => {
                    let mut sub = Parser { t: toks, i: 0 };
                    let e = sub.pipe()?;
                    sub.expect(Lx::End, "')'")?;
                    out.push(StrPart::Expr(e));
                }
            }
        }
        Ok(rc(Ast::Str(out)))
    }

    fn primary(&mut self) -> Result<P, String> {
        match self.bump() {
            Lx::Dot => match self.peek() {
                Lx::Str(_) => {
                    let key = self.string_literal()?;
                    Ok(index(rc(Ast::Identity), key))
                }
                Lx::LBrack => self.bracket(rc(Ast::Identity)),
                _ => Ok(rc(Ast::Identity)),
            },
            Lx::DotDot => Ok(rc(Ast::Recurse)),
            Lx::Field(name) => Ok(index(rc(Ast::Identity), lit_str(&name))),
            Lx::Num(n) => Ok(rc(Ast::Literal(Value::Number(n)))),
            Lx::Str(parts) => self.string_from(parts),
            Lx::Var(name) => Ok(rc(Ast::Var(name))),
            Lx::LParen => {
                let e = self.pipe()?;
                self.expect(Lx::RParen, "')'")?;
                Ok(e)
            }
            Lx::LBrack => {
                if *self.peek() == Lx::RBrack {
                    self.bump();
                    return Ok(rc(Ast::Array(None)));
                }
                let e = self.pipe()?;
                self.expect(Lx::RBrack, "']'")?;
                Ok(rc(Ast::Array(Some(e))))
            }
            Lx::LBrace => self.object(),
            Lx::Ident(name) => match name.as_str() {
                "if" => self.if_rest(),
                "try" => {
                    let body = self.postfix()?;
                    let handler = if self.is_kw("catch") {
                        self.bump();
                        Some(self.postfix()?)
                    } else {
                        None
                    };
                    Ok(rc(Ast::Try(body, handler)))
                }
                "reduce" | "foreach" => {
                    let src = self.postfix()?;
                    self.expect_kw("as")?;
                    let var = self.var_name()?;
                    self.expect(Lx::LParen, "'('")?;
                    let init = self.pipe()?;
                    self.expect(Lx::Semi, "';'")?;
                    let update = self.pipe()?;
                    if name == "reduce" {
                        self.expect(Lx::RParen, "')'")?;
                        return Ok(rc(Ast::Reduce(src, var, init, update)));
                    }
                    let extract = if *self.peek() == Lx::Semi {
                        self.bump();
                        Some(self.pipe()?)
                    } else {
                        None
                    };
                    self.expect(Lx::RParen, "')'")?;
                    Ok(rc(Ast::Foreach(src, var, init, update, extract)))
                }
                "true" => Ok(rc(Ast::Literal(Value::Bool(true)))),
                "false" => Ok(rc(Ast::Literal(Value::Bool(false)))),
                "null" => Ok(rc(Ast::Literal(Value::Null))),
                kw if KEYWORDS.contains(&kw) => Err(format!("syntax error: unexpected keyword '{}'", kw)),
                _ => {
                    let mut args = Vec::new();
                    if *self.peek() == Lx::LParen {
                        self.bump();
                        loop {
                            args.push(self.pipe()?);
                            match self.bump() {
                                Lx::Semi => continue,
                                Lx::RParen => break,
                                other => {
                                    return Err(format!(
                                        "syntax error: expected ';' or ')' in arguments of {} but found {}",
                                        name,
                                        describe(&other)
                                    ))
                                }
                            }
                        }
                    }
                    Ok(rc(Ast::Call(name, args)))
                }
            },
            other => Err(format!("syntax error: unexpected {}", describe(&other))),
        }
    }

    /// Parses the rest of `if` (or `elif`) up to and including `end`.
    fn if_rest(&mut self) -> Result<P, String> {
        let cond = self.pipe()?;
        self.expect_kw("then")?;
        let then = self.pipe()?;
        let otherwise = if self.is_kw("elif") {
            self.bump();
            Some(self.if_rest()?)
        } else if self.is_kw("else") {
            self.bump();
            let e = self.pipe()?;
            self.expect_kw("end")?;
            Some(e)
        } else {
            self.expect_kw("end")?;
            None
        };
        Ok(rc(Ast::If(cond, then, otherwise)))
    }

    fn object(&mut self) -> Result<P, String> {
        let mut entries = Vec::new();
        if *self.peek() == Lx::RBrace {
            self.bump();
            return Ok(rc(Ast::Object(entries)));
        }
        loop {
            let (key, shorthand): (P, Option<P>) = match self.bump() {
                Lx::Var(n) => (lit_str(&n), Some(rc(Ast::Var(n)))),
                Lx::Ident(n) => (lit_str(&n), Some(index(rc(Ast::Identity), lit_str(&n)))),
                Lx::Str(parts) => {
                    let k = self.string_from(parts)?;
                    (k.clone(), Some(index(rc(Ast::Identity), k)))
                }
                Lx::LParen => {
                    let k = self.pipe()?;
                    self.expect(Lx::RParen, "')'")?;
                    (k, None)
                }
                other => return Err(format!("syntax error: unexpected {} in object construction", describe(&other))),
            };
            let value = if *self.peek() == Lx::Colon {
                self.bump();
                self.object_value()?
            } else {
                match shorthand {
                    Some(v) => v,
                    None => return self.unexpected("':'"),
                }
            };
            entries.push((key, value));
            match self.bump() {
                Lx::Comma => continue,
                Lx::RBrace => break,
                other => return Err(format!("syntax error: expected ',' or '}}' but found {}", describe(&other))),
            }
        }
        Ok(rc(Ast::Object(entries)))
    }

    /// An object value: any expression without a top-level comma.
    fn object_value(&mut self) -> Result<P, String> {
        let first = self.unary()?;
        let mut v = self.binary_rest(first, 2)?;
        while *self.peek() == Lx::Pipe {
            self.bump();
            let next = self.unary()?;
            let r = self.binary_rest(next, 2)?;
            v = rc(Ast::Pipe(v, r));
        }
        Ok(v)
    }

    fn funcdef(&mut self) -> Result<Rc<FuncDef>, String> {
        self.expect_kw("def")?;
        let name = match self.bump() {
            Lx::Ident(n) if !KEYWORDS.contains(&n.as_str()) => n,
            other => return Err(format!("syntax error: expected a function name but found {}", describe(&other))),
        };
        let mut params = Vec::new();
        if *self.peek() == Lx::LParen {
            self.bump();
            loop {
                match self.bump() {
                    Lx::Ident(n) => params.push(Param::Filter(n)),
                    Lx::Var(n) => params.push(Param::Value(n)),
                    other => return Err(format!("syntax error: bad parameter {}", describe(&other))),
                }
                match self.bump() {
                    Lx::Semi => continue,
                    Lx::RParen => break,
                    other => return Err(format!("syntax error: expected ';' or ')' but found {}", describe(&other))),
                }
            }
        }
        self.expect(Lx::Colon, "':'")?;
        let body = self.pipe()?;
        self.expect(Lx::Semi, "';'")?;
        Ok(Rc::new(FuncDef { name, params, body }))
    }
}

fn make_binop(op: Lx, l: P, r: P) -> P {
    rc(match op {
        Lx::Comma => Ast::Comma(l, r),
        Lx::Ident(s) if s == "or" => Ast::Or(l, r),
        Lx::Ident(_) => Ast::And(l, r),
        Lx::Op(o) => match o {
            "//" => Ast::Alt(l, r),
            "=" => Ast::Assign(AssignOp::Set, l, r),
            "|=" => Ast::Assign(AssignOp::Update, l, r),
            "//=" => Ast::Assign(AssignOp::Alt, l, r),
            "+=" => Ast::Assign(AssignOp::Arith(BinOp::Add), l, r),
            "-=" => Ast::Assign(AssignOp::Arith(BinOp::Sub), l, r),
            "*=" => Ast::Assign(AssignOp::Arith(BinOp::Mul), l, r),
            "/=" => Ast::Assign(AssignOp::Arith(BinOp::Div), l, r),
            "%=" => Ast::Assign(AssignOp::Arith(BinOp::Mod), l, r),
            "+" => Ast::Binop(BinOp::Add, l, r),
            "-" => Ast::Binop(BinOp::Sub, l, r),
            "*" => Ast::Binop(BinOp::Mul, l, r),
            "/" => Ast::Binop(BinOp::Div, l, r),
            "%" => Ast::Binop(BinOp::Mod, l, r),
            "==" => Ast::Binop(BinOp::Eq, l, r),
            "!=" => Ast::Binop(BinOp::Ne, l, r),
            "<" => Ast::Binop(BinOp::Lt, l, r),
            "<=" => Ast::Binop(BinOp::Le, l, r),
            ">" => Ast::Binop(BinOp::Gt, l, r),
            ">=" => Ast::Binop(BinOp::Ge, l, r),
            _ => unreachable!("unknown operator {}", o),
        },
        _ => unreachable!(),
    })
}

fn lex(src: &str) -> Result<Vec<Lx>, String> {
    Lexer { c: src.chars().collect(), i: 0 }.lex_all(false)
}

pub fn parse_program(src: &str) -> Result<P, String> {
    let mut p = Parser { t: lex(src)?, i: 0 };
    let ast = p.pipe()?;
    if *p.peek() != Lx::End {
        return p.unexpected("end of program");
    }
    Ok(ast)
}

/// Parses a sequence of `def`s (used for the builtin prelude).
pub fn parse_defs(src: &str) -> Result<Vec<Rc<FuncDef>>, String> {
    let mut p = Parser { t: lex(src)?, i: 0 };
    let mut defs = Vec::new();
    while *p.peek() != Lx::End {
        defs.push(p.funcdef()?);
    }
    Ok(defs)
}
