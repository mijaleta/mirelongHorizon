//! Evaluator. Generators are implemented in continuation-passing style: every
//! expression calls `out` once per output, which keeps infinite generators lazy
//! and lets `limit`, `first` and `try` stop them early.

use crate::parser::*;
use crate::prelude::PRELUDE;
use crate::value::*;
use std::cell::Cell;
use std::rc::Rc;

pub enum Err {
    /// A jq error carrying its error value.
    Val(Value),
    /// Internal unwinding used to stop a generator early.
    Break(usize),
}

pub type R = Result<(), Err>;
type Out<'a> = &'a mut dyn FnMut(Value) -> R;
type PathOut<'a> = &'a mut dyn FnMut(Vec<Value>, Value) -> R;

pub fn fail<T>(msg: impl Into<String>) -> Result<T, Err> {
    Err(Err::Val(Value::string(&msg.into())))
}

thread_local! {
    static LABELS: Cell<usize> = const { Cell::new(0) };
}

fn fresh_label() -> usize {
    LABELS.with(|c| {
        c.set(c.get() + 1);
        c.get()
    })
}

/// "type (value)" with the value truncated, as used in jq's error messages.
fn desc(v: &Value) -> String {
    let mut dump = to_json(v, false);
    if dump.len() > 14 {
        let mut cut = 11;
        while !dump.is_char_boundary(cut) {
            cut -= 1;
        }
        dump.truncate(cut);
        dump.push_str("...");
    }
    format!("{} ({})", v.type_name(), dump)
}

// ---------------------------------------------------------------------------
// Environments

pub struct Env {
    bind: Bind,
    parent: Option<Rc<Env>>,
}

enum Bind {
    Root,
    Var(String, Value),
    Func(Rc<FuncDef>),
    /// A filter argument: its expression plus the caller's environment.
    Param(String, P, Rc<Env>),
}

enum Found {
    Def(Rc<FuncDef>, Rc<Env>),
    Param(P, Rc<Env>),
}

impl Env {
    pub fn root() -> Rc<Env> {
        Rc::new(Env { bind: Bind::Root, parent: None })
    }
}

fn push(env: &Rc<Env>, bind: Bind) -> Rc<Env> {
    Rc::new(Env { bind, parent: Some(env.clone()) })
}

pub fn bind_var(env: &Rc<Env>, name: &str, v: Value) -> Rc<Env> {
    push(env, Bind::Var(name.to_string(), v))
}

fn lookup_var(env: &Rc<Env>, name: &str) -> Value {
    let mut e = env;
    loop {
        if let Bind::Var(n, v) = &e.bind {
            if n == name {
                return v.clone();
            }
        }
        match &e.parent {
            Some(p) => e = p,
            None => return Value::Null,
        }
    }
}

fn lookup_func(env: &Rc<Env>, name: &str, arity: usize) -> Option<Found> {
    let mut e = env;
    loop {
        match &e.bind {
            Bind::Func(fd) if fd.name == name && fd.params.len() == arity => {
                return Some(Found::Def(fd.clone(), e.clone()));
            }
            Bind::Param(n, body, cenv) if arity == 0 && n == name => {
                return Some(Found::Param(body.clone(), cenv.clone()));
            }
            _ => {}
        }
        match &e.parent {
            Some(p) => e = p,
            None => return None,
        }
    }
}

// ---------------------------------------------------------------------------
// Compilation: parse, attach the prelude, and check that every name resolves.

const NATIVES: &[(&str, usize)] = &[
    ("empty", 0),
    ("error", 0),
    ("error", 1),
    ("length", 0),
    ("utf8bytelength", 0),
    ("keys", 0),
    ("keys_unsorted", 0),
    ("type", 0),
    ("tostring", 0),
    ("tojson", 0),
    ("fromjson", 0),
    ("tonumber", 0),
    ("sort", 0),
    ("sort_by", 1),
    ("group_by", 1),
    ("unique", 0),
    ("unique_by", 1),
    ("min", 0),
    ("max", 0),
    ("min_by", 1),
    ("max_by", 1),
    ("reverse", 0),
    ("floor", 0),
    ("ceil", 0),
    ("round", 0),
    ("sqrt", 0),
    ("explode", 0),
    ("implode", 0),
    ("ascii_downcase", 0),
    ("ascii_upcase", 0),
    ("flatten", 0),
    ("flatten", 1),
    ("has", 1),
    ("contains", 1),
    ("split", 1),
    ("startswith", 1),
    ("endswith", 1),
    ("ltrimstr", 1),
    ("rtrimstr", 1),
    ("path", 1),
    ("getpath", 1),
    ("setpath", 2),
    ("delpaths", 1),
    ("first", 1),
    ("limit", 2),
    ("range", 2),
];

enum ScopeItem<'a> {
    Root(&'a [String]),
    Var(&'a str),
    Func(&'a str, usize),
}

struct Scope<'a> {
    item: ScopeItem<'a>,
    parent: Option<&'a Scope<'a>>,
}

impl<'a> Scope<'a> {
    fn has_var(&self, name: &str) -> bool {
        let mut s = self;
        loop {
            match &s.item {
                ScopeItem::Var(n) if *n == name => return true,
                ScopeItem::Root(vars) if vars.iter().any(|v| v == name) => return true,
                _ => {}
            }
            match s.parent {
                Some(p) => s = p,
                None => return false,
            }
        }
    }

    fn has_func(&self, name: &str, arity: usize) -> bool {
        let mut s = self;
        loop {
            if let ScopeItem::Func(n, a) = &s.item {
                if *n == name && *a == arity {
                    return true;
                }
            }
            match s.parent {
                Some(p) => s = p,
                None => return NATIVES.contains(&(name, arity)),
            }
        }
    }
}

fn check(ast: &Ast, sc: &Scope) -> Result<(), String> {
    match ast {
        Ast::Identity | Ast::Recurse | Ast::Literal(_) => Ok(()),
        Ast::Str(parts) => {
            for p in parts {
                if let StrPart::Expr(e) = p {
                    check(e, sc)?;
                }
            }
            Ok(())
        }
        Ast::Index(a, b)
        | Ast::Pipe(a, b)
        | Ast::Comma(a, b)
        | Ast::Binop(_, a, b)
        | Ast::And(a, b)
        | Ast::Or(a, b)
        | Ast::Alt(a, b)
        | Ast::Assign(_, a, b) => {
            check(a, sc)?;
            check(b, sc)
        }
        Ast::Slice(t, from, to) => {
            check(t, sc)?;
            for e in [from, to].into_iter().flatten() {
                check(e, sc)?;
            }
            Ok(())
        }
        Ast::Iterate(e) | Ast::Neg(e) | Ast::Array(Some(e)) => check(e, sc),
        Ast::Array(None) => Ok(()),
        Ast::Object(entries) => {
            for (k, v) in entries {
                check(k, sc)?;
                check(v, sc)?;
            }
            Ok(())
        }
        Ast::If(c, t, e) => {
            check(c, sc)?;
            check(t, sc)?;
            match e {
                Some(e) => check(e, sc),
                None => Ok(()),
            }
        }
        Ast::Try(b, h) => {
            check(b, sc)?;
            match h {
                Some(h) => check(h, sc),
                None => Ok(()),
            }
        }
        Ast::Reduce(src, var, init, update) => {
            check(src, sc)?;
            check(init, sc)?;
            check(update, &Scope { item: ScopeItem::Var(var), parent: Some(sc) })
        }
        Ast::Foreach(src, var, init, update, extract) => {
            check(src, sc)?;
            check(init, sc)?;
            let inner = Scope { item: ScopeItem::Var(var), parent: Some(sc) };
            check(update, &inner)?;
            match extract {
                Some(x) => check(x, &inner),
                None => Ok(()),
            }
        }
        Ast::As(src, var, body) => {
            check(src, sc)?;
            check(body, &Scope { item: ScopeItem::Var(var), parent: Some(sc) })
        }
        Ast::Def(fd, rest) => {
            let with_self = Scope { item: ScopeItem::Func(&fd.name, fd.params.len()), parent: Some(sc) };
            check_params(fd, 0, &with_self)?;
            check(rest, &with_self)
        }
        Ast::Call(name, args) => {
            if !sc.has_func(name, args.len()) {
                return Err(format!("{}/{} is not defined", name, args.len()));
            }
            for a in args {
                check(a, sc)?;
            }
            Ok(())
        }
        Ast::Var(name) => {
            if sc.has_var(name) {
                Ok(())
            } else {
                Err(format!("${} is not defined", name))
            }
        }
    }
}

fn check_params(fd: &FuncDef, i: usize, sc: &Scope) -> Result<(), String> {
    if i == fd.params.len() {
        return check(&fd.body, sc);
    }
    match &fd.params[i] {
        Param::Filter(n) => check_params(fd, i + 1, &Scope { item: ScopeItem::Func(n, 0), parent: Some(sc) }),
        Param::Value(n) => {
            let f = Scope { item: ScopeItem::Func(n, 0), parent: Some(sc) };
            check_params(fd, i + 1, &Scope { item: ScopeItem::Var(n), parent: Some(&f) })
        }
    }
}

pub fn compile(src: &str, arg_names: &[String]) -> Result<P, String> {
    let mut ast = parse_program(src)?;
    let defs = parse_defs(PRELUDE).expect("prelude must parse");
    for fd in defs.into_iter().rev() {
        ast = Rc::new(Ast::Def(fd, ast));
    }
    check(&ast, &Scope { item: ScopeItem::Root(arg_names), parent: None })?;
    Ok(ast)
}

// ---------------------------------------------------------------------------
// Value operations

pub fn index(v: &Value, k: &Value) -> Result<Value, Err> {
    match (v, k) {
        (Value::Object(m), Value::String(s)) => Ok(obj_get(m, s).cloned().unwrap_or(Value::Null)),
        (Value::Array(a), Value::Number(n)) => {
            let mut i = n.floor();
            if i < 0.0 {
                i += a.len() as f64;
            }
            if i < 0.0 || i >= a.len() as f64 {
                Ok(Value::Null)
            } else {
                Ok(a[i as usize].clone())
            }
        }
        (Value::Null, Value::String(_) | Value::Number(_)) => Ok(Value::Null),
        (_, Value::String(s)) => fail(format!("Cannot index {} with \"{}\"", v.type_name(), s)),
        _ => fail(format!("Cannot index {} with {}", v.type_name(), k.type_name())),
    }
}

fn slice_bound(b: &Value, default: f64, len: f64) -> Result<f64, Err> {
    match b {
        Value::Null => Ok(default),
        Value::Number(n) => {
            let mut x = *n;
            if x < 0.0 {
                x += len;
            }
            Ok(x.clamp(0.0, len))
        }
        _ => fail("Start and end indices of an array slice must be numbers"),
    }
}

fn slice(v: &Value, from: &Value, to: &Value) -> Result<Value, Err> {
    let len = match v {
        Value::Null => return Ok(Value::Null),
        Value::Array(a) => a.len(),
        Value::String(s) => s.chars().count(),
        _ => return fail(format!("Cannot index {} with object", v.type_name())),
    } as f64;
    let start = slice_bound(from, 0.0, len)?.floor() as usize;
    let end = (slice_bound(to, len, len)?.ceil() as usize).max(start);
    Ok(match v {
        Value::Array(a) => Value::array(a[start..end].to_vec()),
        Value::String(s) => Value::string(&s.chars().skip(start).take(end - start).collect::<String>()),
        _ => unreachable!(),
    })
}

fn cannot_iterate(v: &Value) -> String {
    match v {
        Value::Null => "Cannot iterate over null".to_string(),
        _ => format!("Cannot iterate over {}", desc(v)),
    }
}

fn iterate(v: &Value, out: Out) -> R {
    match v {
        Value::Array(a) => a.iter().try_for_each(|x| out(x.clone())),
        Value::Object(m) => m.iter().try_for_each(|(_, x)| out(x.clone())),
        _ => fail(cannot_iterate(v)),
    }
}

fn recurse_values(v: &Value, out: Out) -> R {
    out(v.clone())?;
    match v {
        Value::Array(a) => a.iter().try_for_each(|x| recurse_values(x, out)),
        Value::Object(m) => m.iter().try_for_each(|(_, x)| recurse_values(x, out)),
        _ => Ok(()),
    }
}

fn deep_merge(a: &Map, b: &Map) -> Map {
    let mut m = a.clone();
    for (k, bv) in b {
        let merged = match (obj_get(&m, k), bv) {
            (Some(Value::Object(x)), Value::Object(y)) => Value::object(deep_merge(x, y)),
            _ => bv.clone(),
        };
        obj_set(&mut m, k, merged);
    }
    m
}

fn split_string(s: &str, sep: &str) -> Value {
    if s.is_empty() {
        return Value::array(Vec::new());
    }
    if sep.is_empty() {
        return Value::array(s.chars().map(|c| Value::string(&c.to_string())).collect());
    }
    Value::array(s.split(sep).map(Value::string).collect())
}

pub fn binop(op: BinOp, l: Value, r: Value) -> Result<Value, Err> {
    use Value::*;
    let cmp = |want: &dyn Fn(std::cmp::Ordering) -> bool| Ok(Bool(want(compare(&l, &r))));
    match op {
        BinOp::Eq => return cmp(&|o| o.is_eq()),
        BinOp::Ne => return cmp(&|o| o.is_ne()),
        BinOp::Lt => return cmp(&|o| o.is_lt()),
        BinOp::Le => return cmp(&|o| o.is_le()),
        BinOp::Gt => return cmp(&|o| o.is_gt()),
        BinOp::Ge => return cmp(&|o| o.is_ge()),
        _ => {}
    }
    match (op, &l, &r) {
        (BinOp::Add, Null, _) => Ok(r),
        (BinOp::Add, _, Null) => Ok(l),
        (BinOp::Add, Number(a), Number(b)) => Ok(Number(a + b)),
        (BinOp::Add, String(a), String(b)) => Ok(Value::string(&format!("{}{}", a, b))),
        (BinOp::Add, Array(a), Array(b)) => Ok(Value::array(a.iter().chain(b.iter()).cloned().collect())),
        (BinOp::Add, Object(a), Object(b)) => {
            let mut m = (**a).clone();
            for (k, v) in b.iter() {
                obj_set(&mut m, k, v.clone());
            }
            Ok(Value::object(m))
        }
        (BinOp::Sub, Number(a), Number(b)) => Ok(Number(a - b)),
        (BinOp::Sub, Array(a), Array(b)) => {
            Ok(Value::array(a.iter().filter(|x| !b.iter().any(|y| equal(x, y))).cloned().collect()))
        }
        (BinOp::Mul, Number(a), Number(b)) => Ok(Number(a * b)),
        (BinOp::Mul, String(s), Number(n)) | (BinOp::Mul, Number(n), String(s)) => {
            if *n < 0.0 {
                Ok(Null)
            } else {
                Ok(Value::string(&s.repeat(*n as usize)))
            }
        }
        (BinOp::Mul, Object(a), Object(b)) => Ok(Value::object(deep_merge(a, b))),
        (BinOp::Div, Number(a), Number(b)) => {
            if *b == 0.0 {
                fail(format!("{} and {} cannot be divided because the divisor is zero", desc(&l), desc(&r)))
            } else {
                Ok(Number(a / b))
            }
        }
        (BinOp::Div, String(a), String(b)) => Ok(split_string(a, b)),
        (BinOp::Mod, Number(a), Number(b)) => {
            let (ai, bi) = (*a as i64, *b as i64);
            if bi == 0 {
                fail(format!("{} and {} cannot be divided because the divisor is zero", desc(&l), desc(&r)))
            } else {
                Ok(Number(ai.wrapping_rem(bi) as f64))
            }
        }
        _ => {
            let verb = match op {
                BinOp::Add => "added",
                BinOp::Sub => "subtracted",
                BinOp::Mul => "multiplied",
                BinOp::Div => "divided",
                _ => "divided",
            };
            fail(format!("{} and {} cannot be {}", desc(&l), desc(&r), verb))
        }
    }
}

fn contains(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            y.iter().all(|(k, bv)| obj_get(x, k).map_or(false, |av| contains(av, bv)))
        }
        (Value::Array(x), Value::Array(y)) => y.iter().all(|bv| x.iter().any(|av| contains(av, bv))),
        (Value::String(x), Value::String(y)) => x.contains(&**y),
        _ => a.type_name() == b.type_name() && equal(a, b),
    }
}

fn as_path(p: &Value) -> Result<Vec<Value>, Err> {
    match p {
        Value::Array(a) => Ok((**a).clone()),
        _ => fail("Path must be specified as an array"),
    }
}

pub fn getpath(v: &Value, path: &[Value]) -> Result<Value, Err> {
    let mut cur = v.clone();
    for k in path {
        cur = index(&cur, k)?;
    }
    Ok(cur)
}

pub fn setpath(v: Value, path: &[Value], nv: Value) -> Result<Value, Err> {
    let Some((k, rest)) = path.split_first() else {
        return Ok(nv);
    };
    match (v, k) {
        (Value::Null, Value::String(s)) => {
            let child = setpath(Value::Null, rest, nv)?;
            Ok(Value::object(vec![(s.to_string(), child)]))
        }
        (Value::Object(mut m), Value::String(s)) => {
            let mm = Rc::make_mut(&mut m);
            match mm.iter().position(|(key, _)| key.as_str() == &**s) {
                Some(i) => {
                    let old = std::mem::replace(&mut mm[i].1, Value::Null);
                    mm[i].1 = setpath(old, rest, nv)?;
                }
                None => {
                    let child = setpath(Value::Null, rest, nv)?;
                    mm.push((s.to_string(), child));
                }
            }
            Ok(Value::Object(m))
        }
        (v @ (Value::Null | Value::Array(_)), Value::Number(n)) => {
            let mut a = match v {
                Value::Array(a) => a,
                _ => Rc::new(Vec::new()),
            };
            let am = Rc::make_mut(&mut a);
            let mut i = n.floor() as i64;
            if i < 0 {
                i += am.len() as i64;
                if i < 0 {
                    return fail("Out of bounds negative array index");
                }
            }
            let i = i as usize;
            if i >= am.len() {
                am.resize(i + 1, Value::Null);
            }
            let old = std::mem::replace(&mut am[i], Value::Null);
            am[i] = setpath(old, rest, nv)?;
            Ok(Value::Array(a))
        }
        (v, Value::String(s)) => fail(format!("Cannot index {} with \"{}\"", v.type_name(), s)),
        (v, k) => fail(format!("Cannot index {} with {}", v.type_name(), k.type_name())),
    }
}

fn delpath(v: Value, path: &[Value]) -> Result<Value, Err> {
    let Some((k, rest)) = path.split_first() else {
        return Ok(Value::Null);
    };
    if matches!(v, Value::Null) {
        return Ok(Value::Null);
    }
    if rest.is_empty() {
        return match (v, k) {
            (Value::Object(mut m), Value::String(s)) => {
                Rc::make_mut(&mut m).retain(|(key, _)| key.as_str() != &**s);
                Ok(Value::Object(m))
            }
            (Value::Array(mut a), Value::Number(n)) => {
                let len = a.len() as i64;
                let mut i = n.floor() as i64;
                if i < 0 {
                    i += len;
                    if i < 0 {
                        return fail("Out of bounds negative array index");
                    }
                }
                if i < len {
                    Rc::make_mut(&mut a).remove(i as usize);
                }
                Ok(Value::Array(a))
            }
            (v, k) => fail(format!("Cannot delete field at {} index of {}", k.type_name(), v.type_name())),
        };
    }
    let child = index(&v, k)?;
    if matches!(child, Value::Null) {
        return Ok(v);
    }
    let new_child = delpath(child, rest)?;
    setpath(v, std::slice::from_ref(k), new_child)
}

pub fn delpaths(v: Value, ps: &Value) -> Result<Value, Err> {
    let Value::Array(list) = ps else {
        return fail("Paths must be specified as an array");
    };
    let mut paths = Vec::new();
    for p in list.iter() {
        match p {
            Value::Array(_) => paths.push(p.clone()),
            _ => return fail("Path must be specified as an array"),
        }
    }
    paths.sort_by(compare);
    let mut v = v;
    for p in paths.iter().rev() {
        let Value::Array(p) = p else { unreachable!() };
        v = delpath(v, p)?;
    }
    Ok(v)
}

fn flatten(a: &[Value], depth: f64, out: &mut Vec<Value>) {
    for x in a {
        match x {
            Value::Array(inner) if depth > 0.0 => flatten(inner, depth - 1.0, out),
            _ => out.push(x.clone()),
        }
    }
}

fn number_arg(v: &Value, what: &str) -> Result<f64, Err> {
    match v {
        Value::Number(n) => Ok(*n),
        _ => fail(format!("{} must be a number, got {}", what, desc(v))),
    }
}

fn string_input<'a>(v: &'a Value, name: &str) -> Result<&'a str, Err> {
    match v {
        Value::String(s) => Ok(s),
        _ => fail(format!("{} input must be a string", name)),
    }
}

fn native0(name: &str, v: Value) -> Result<Value, Err> {
    use Value::*;
    Ok(match name {
        "length" => match &v {
            Null => Number(0.0),
            Bool(_) => return fail(format!("{} has no length", desc(&v))),
            Number(n) => Number(n.abs()),
            String(s) => Number(s.chars().count() as f64),
            Array(a) => Number(a.len() as f64),
            Object(m) => Number(m.len() as f64),
        },
        "utf8bytelength" => match &v {
            String(s) => Number(s.len() as f64),
            _ => return fail(format!("{} only strings have UTF-8 byte length", desc(&v))),
        },
        "keys" | "keys_unsorted" => match &v {
            Object(m) if name == "keys" => Value::array(sorted_keys(m).into_iter().map(Value::string).collect()),
            Object(m) => Value::array(m.iter().map(|(k, _)| Value::string(k)).collect()),
            Array(a) => Value::array((0..a.len()).map(|i| Number(i as f64)).collect()),
            _ => return fail(format!("{} has no keys", desc(&v))),
        },
        "type" => Value::string(v.type_name()),
        "tostring" => match &v {
            String(_) => v,
            _ => Value::string(&to_json(&v, false)),
        },
        "tojson" => Value::string(&to_json(&v, false)),
        "fromjson" => {
            let s = string_input(&v, "fromjson")?;
            match parse_single(s) {
                Ok(x) => x,
                Err(e) => return fail(format!("{} (while parsing '{}')", e, s)),
            }
        }
        "tonumber" => match &v {
            Number(_) => v,
            String(s) => match parse_single(s) {
                Ok(n @ Number(_)) => n,
                _ => return fail(format!("Cannot parse '{}' as a number", s)),
            },
            _ => return fail(format!("{} cannot be parsed as a number", desc(&v))),
        },
        "sort" | "unique" | "min" | "max" => {
            let Array(a) = &v else {
                return fail(format!("{} cannot be sorted, as it is not an array", desc(&v)));
            };
            let keyed: Vec<(Value, Value)> = a.iter().map(|x| (x.clone(), x.clone())).collect();
            let by = match name {
                "sort" => "sort_by",
                "unique" => "unique_by",
                "min" => "min_by",
                _ => "max_by",
            };
            by_impl(by, keyed)
        }
        // jq defines reverse as `[.[length - 1 - range(0; length)]]`.
        "reverse" => match &v {
            Array(a) => Value::array(a.iter().rev().cloned().collect()),
            _ => match native0("length", v.clone())? {
                Number(n) if n == 0.0 => Value::array(Vec::new()),
                _ => return fail(format!("Cannot index {} with number", v.type_name())),
            },
        },
        "floor" | "ceil" | "round" | "sqrt" => {
            let n = number_arg(&v, name)?;
            Number(match name {
                "floor" => n.floor(),
                "ceil" => n.ceil(),
                "round" => n.round(),
                _ => n.sqrt(),
            })
        }
        "explode" => {
            let s = string_input(&v, "explode")?;
            Value::array(s.chars().map(|c| Number(c as u32 as f64)).collect())
        }
        "implode" => {
            let Array(a) = &v else {
                return fail("Implode input must be an array");
            };
            let mut s = std::string::String::new();
            for x in a.iter() {
                let Number(n) = x else {
                    return fail("Unicode codepoint must be numeric");
                };
                match char::from_u32(*n as u32) {
                    Some(c) => s.push(c),
                    None => return fail("Invalid codepoint literal"),
                }
            }
            Value::string(&s)
        }
        "ascii_downcase" => Value::string(&string_input(&v, "ascii_downcase")?.to_ascii_lowercase()),
        "ascii_upcase" => Value::string(&string_input(&v, "ascii_upcase")?.to_ascii_uppercase()),
        "flatten" => native1("flatten", &v, &Number(1e9))?,
        _ => unreachable!("unknown native {}/0", name),
    })
}

fn native1(name: &str, input: &Value, arg: &Value) -> Result<Value, Err> {
    use Value::*;
    Ok(match name {
        "has" => match (input, arg) {
            (Object(m), String(k)) => Bool(obj_get(m, k).is_some()),
            (Array(a), Number(n)) => Bool(*n >= 0.0 && *n < a.len() as f64),
            _ => return fail(format!("Cannot check whether {} has a {} key", input.type_name(), arg.type_name())),
        },
        "contains" => {
            if input.type_name() != arg.type_name() {
                return fail(format!("{} and {} cannot have their containment checked", desc(input), desc(arg)));
            }
            Bool(contains(input, arg))
        }
        "split" => match (input, arg) {
            (String(s), String(sep)) => split_string(s, sep),
            _ => return fail("split input and separator must be strings"),
        },
        "startswith" | "endswith" => match (input, arg) {
            (String(s), String(t)) => Bool(if name == "startswith" { s.starts_with(&**t) } else { s.ends_with(&**t) }),
            _ => return fail(format!("{}() requires string inputs", name)),
        },
        "ltrimstr" | "rtrimstr" => match (input, arg) {
            (String(s), String(t)) => {
                let stripped = if name == "ltrimstr" { s.strip_prefix(&**t) } else { s.strip_suffix(&**t) };
                match stripped {
                    Some(rest) => Value::string(rest),
                    None => input.clone(),
                }
            }
            _ => input.clone(),
        },
        "flatten" => {
            let depth = number_arg(arg, "flatten depth")?;
            if depth < 0.0 {
                return fail("flatten depth must not be negative");
            }
            let Array(a) = input else {
                return fail(cannot_iterate(input));
            };
            let mut out = Vec::new();
            flatten(a, depth, &mut out);
            Value::array(out)
        }
        _ => unreachable!("unknown native {}/1", name),
    })
}

/// sort_by / group_by / unique_by / min_by / max_by over (key, value) pairs.
fn by_impl(name: &str, mut keyed: Vec<(Value, Value)>) -> Value {
    match name {
        "min_by" | "max_by" => {
            let mut best: Option<(Value, Value)> = None;
            for (k, v) in keyed {
                let replace = match &best {
                    None => true,
                    Some((bk, _)) => {
                        let c = compare(&k, bk);
                        if name == "min_by" {
                            c.is_lt()
                        } else {
                            !c.is_lt()
                        }
                    }
                };
                if replace {
                    best = Some((k, v));
                }
            }
            best.map(|(_, v)| v).unwrap_or(Value::Null)
        }
        _ => {
            keyed.sort_by(|a, b| compare(&a.0, &b.0));
            if name == "sort_by" {
                return Value::array(keyed.into_iter().map(|(_, v)| v).collect());
            }
            let mut groups: Vec<(Value, Vec<Value>)> = Vec::new();
            for (k, v) in keyed {
                match groups.last_mut() {
                    Some((gk, g)) if equal(gk, &k) => g.push(v),
                    _ => groups.push((k, vec![v])),
                }
            }
            if name == "group_by" {
                Value::array(groups.into_iter().map(|(_, g)| Value::array(g)).collect())
            } else {
                Value::array(groups.into_iter().map(|(_, mut g)| g.swap_remove(0)).collect())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Evaluation

/// Runs `body` with a sink forwarding to `out`. Errors raised by `out` (that is,
/// by whatever consumes our outputs) are handed back separately so that `try`
/// and `//` only ever catch errors raised by their own operand.
fn guarded(out: Out, body: impl FnOnce(Out) -> R) -> (R, Option<Err>) {
    let id = fresh_label();
    let mut downstream = None;
    let r = body(&mut |v| match out(v) {
        Ok(()) => Ok(()),
        Err(e) => {
            downstream = Some(e);
            Err(Err::Break(id))
        }
    });
    (r, downstream)
}

fn guarded_paths(out: PathOut, body: impl FnOnce(PathOut) -> R) -> (R, Option<Err>) {
    let id = fresh_label();
    let mut downstream = None;
    let r = body(&mut |p, v| match out(p, v) {
        Ok(()) => Ok(()),
        Err(e) => {
            downstream = Some(e);
            Err(Err::Break(id))
        }
    });
    (r, downstream)
}

fn eval_opt(e: &Option<P>, env: &Rc<Env>, input: &Value, out: Out) -> R {
    match e {
        Some(e) => eval(e, env, input.clone(), out),
        None => out(Value::Null),
    }
}

pub fn eval(ast: &Ast, env: &Rc<Env>, input: Value, out: Out) -> R {
    match ast {
        Ast::Identity => out(input),
        Ast::Recurse => recurse_values(&input, out),
        Ast::Literal(v) => out(v.clone()),
        Ast::Str(parts) => interpolate(parts, parts.len(), String::new(), env, &input, out),
        Ast::Index(t, k) => eval(k, env, input.clone(), &mut |kv| {
            eval(t, env, input.clone(), &mut |tv| out(index(&tv, &kv)?))
        }),
        Ast::Slice(t, from, to) => eval_opt(from, env, &input, &mut |fv| {
            eval_opt(to, env, &input, &mut |tv| eval(t, env, input.clone(), &mut |v| out(slice(&v, &fv, &tv)?)))
        }),
        Ast::Iterate(t) => eval(t, env, input, &mut |v| iterate(&v, out)),
        Ast::Array(None) => out(Value::array(Vec::new())),
        Ast::Array(Some(e)) => {
            let mut items = Vec::new();
            eval(e, env, input, &mut |v| {
                items.push(v);
                Ok(())
            })?;
            out(Value::array(items))
        }
        Ast::Object(entries) => build_object(entries, 0, Map::new(), env, &input, out),
        Ast::Neg(e) => eval(e, env, input, &mut |v| match v {
            Value::Number(n) => out(Value::Number(-n)),
            other => fail(format!("{} cannot be negated", desc(&other))),
        }),
        Ast::Pipe(a, b) => eval(a, env, input, &mut |v| eval(b, env, v, out)),
        Ast::Comma(a, b) => {
            eval(a, env, input.clone(), out)?;
            eval(b, env, input, out)
        }
        Ast::Binop(op, a, b) => eval(b, env, input.clone(), &mut |rv| {
            eval(a, env, input.clone(), &mut |lv| out(binop(*op, lv, rv.clone())?))
        }),
        Ast::And(a, b) => eval(a, env, input.clone(), &mut |lv| {
            if !lv.truthy() {
                out(Value::Bool(false))
            } else {
                eval(b, env, input.clone(), &mut |rv| out(Value::Bool(rv.truthy())))
            }
        }),
        Ast::Or(a, b) => eval(a, env, input.clone(), &mut |lv| {
            if lv.truthy() {
                out(Value::Bool(true))
            } else {
                eval(b, env, input.clone(), &mut |rv| out(Value::Bool(rv.truthy())))
            }
        }),
        Ast::Alt(a, b) => {
            let mut any = false;
            let (r, down) = guarded(out, |sink| {
                eval(a, env, input.clone(), &mut |v| {
                    if v.truthy() {
                        any = true;
                        sink(v)
                    } else {
                        Ok(())
                    }
                })
            });
            if let Some(e) = down {
                return Err(e);
            }
            if let Err(Err::Break(id)) = r {
                return Err(Err::Break(id));
            }
            if any {
                Ok(())
            } else {
                eval(b, env, input, out)
            }
        }
        Ast::Assign(op, lhs, rhs) => assign(*op, lhs, rhs, env, input, out),
        Ast::If(c, t, e) => eval(c, env, input.clone(), &mut |cv| {
            if cv.truthy() {
                eval(t, env, input.clone(), out)
            } else {
                match e {
                    Some(e) => eval(e, env, input.clone(), out),
                    None => out(input.clone()),
                }
            }
        }),
        Ast::Try(body, handler) => {
            let (r, down) = guarded(out, |sink| eval(body, env, input.clone(), sink));
            if let Some(e) = down {
                return Err(e);
            }
            match r {
                Err(Err::Val(e)) => match handler {
                    Some(h) => eval(h, env, e, out),
                    None => Ok(()),
                },
                other => other,
            }
        }
        Ast::Reduce(src, var, init, update) => eval(init, env, input.clone(), &mut |init_v| {
            let mut acc = Some(init_v);
            eval(src, env, input.clone(), &mut |x| {
                let inner = bind_var(env, var, x);
                let cur = acc.take().unwrap_or(Value::Null);
                eval(update, &inner, cur, &mut |u| {
                    acc = Some(u);
                    Ok(())
                })
            })?;
            out(acc.unwrap_or(Value::Null))
        }),
        Ast::Foreach(src, var, init, update, extract) => eval(init, env, input.clone(), &mut |init_v| {
            let mut state = Some(init_v);
            eval(src, env, input.clone(), &mut |x| {
                let inner = bind_var(env, var, x);
                let cur = state.take().unwrap_or(Value::Null);
                eval(update, &inner, cur, &mut |u| {
                    state = Some(u.clone());
                    match extract {
                        Some(x) => eval(x, &inner, u, out),
                        None => out(u),
                    }
                })
            })
        }),
        Ast::As(src, var, body) => eval(src, env, input.clone(), &mut |v| {
            let inner = bind_var(env, var, v);
            eval(body, &inner, input.clone(), out)
        }),
        Ast::Def(fd, rest) => eval(rest, &push(env, Bind::Func(fd.clone())), input, out),
        Ast::Call(name, args) => call(name, args, env, input, out),
        Ast::Var(name) => out(lookup_var(env, name)),
    }
}

/// String interpolation. The last interpolated expression varies slowest, as in jq.
fn interpolate(parts: &[StrPart], n: usize, suffix: String, env: &Rc<Env>, input: &Value, out: Out) -> R {
    if n == 0 {
        return out(Value::string(&suffix));
    }
    match &parts[n - 1] {
        StrPart::Lit(s) => interpolate(parts, n - 1, format!("{}{}", s, suffix), env, input, out),
        StrPart::Expr(e) => eval(e, env, input.clone(), &mut |v| {
            let s = match &v {
                Value::String(s) => s.to_string(),
                other => to_json(other, false),
            };
            interpolate(parts, n - 1, format!("{}{}", s, suffix), env, input, out)
        }),
    }
}

fn build_object(entries: &[(P, P)], i: usize, acc: Map, env: &Rc<Env>, input: &Value, out: Out) -> R {
    if i == entries.len() {
        return out(Value::object(acc));
    }
    let (k, v) = &entries[i];
    eval(k, env, input.clone(), &mut |kv| {
        let Value::String(key) = &kv else {
            return fail(format!("Object keys must be strings, got {}", desc(&kv)));
        };
        eval(v, env, input.clone(), &mut |vv| {
            let mut m = acc.clone();
            obj_set(&mut m, key, vv);
            build_object(entries, i + 1, m, env, input, out)
        })
    })
}

/// Binds a user function's parameters (value parameters fan out over their
/// argument's outputs) and hands the resulting environment to `k`.
fn bind_params(
    fd: &FuncDef,
    acc: Rc<Env>,
    args: &[P],
    caller: &Rc<Env>,
    i: usize,
    input: &Value,
    k: &mut dyn FnMut(Rc<Env>) -> R,
) -> R {
    if i == args.len() {
        return k(acc);
    }
    match &fd.params[i] {
        Param::Filter(n) => {
            let e = push(&acc, Bind::Param(n.clone(), args[i].clone(), caller.clone()));
            bind_params(fd, e, args, caller, i + 1, input, k)
        }
        Param::Value(n) => eval(&args[i], caller, input.clone(), &mut |v| {
            let e = push(&acc, Bind::Param(n.clone(), args[i].clone(), caller.clone()));
            let e = bind_var(&e, n, v);
            bind_params(fd, e, args, caller, i + 1, input, k)
        }),
    }
}

fn break_target(r: R, id: usize) -> R {
    match r {
        Err(Err::Break(i)) if i == id => Ok(()),
        other => other,
    }
}

fn call(name: &str, args: &[P], env: &Rc<Env>, input: Value, out: Out) -> R {
    match lookup_func(env, name, args.len()) {
        Some(Found::Def(fd, def_env)) => {
            bind_params(&fd, def_env, args, env, 0, &input, &mut |e| eval(&fd.body, &e, input.clone(), out))
        }
        Some(Found::Param(body, cenv)) => eval(&body, &cenv, input, out),
        None => native(name, args, env, input, out),
    }
}

fn native(name: &str, args: &[P], env: &Rc<Env>, input: Value, out: Out) -> R {
    match (name, args.len()) {
        ("empty", 0) => Ok(()),
        ("error", 0) => Err(Err::Val(input)),
        ("error", 1) => eval(&args[0], env, input, &mut |m| Err(Err::Val(m))),
        ("path", 1) => eval_paths(&args[0], env, Vec::new(), input, &mut |p, _| out(Value::array(p))),
        ("getpath", 1) => eval(&args[0], env, input.clone(), &mut |p| out(getpath(&input, &as_path(&p)?)?)),
        ("setpath", 2) => eval(&args[1], env, input.clone(), &mut |v| {
            eval(&args[0], env, input.clone(), &mut |p| out(setpath(input.clone(), &as_path(&p)?, v.clone())?))
        }),
        ("delpaths", 1) => eval(&args[0], env, input.clone(), &mut |ps| out(delpaths(input.clone(), &ps)?)),
        ("first", 1) => {
            let id = fresh_label();
            break_target(
                eval(&args[0], env, input, &mut |v| {
                    out(v)?;
                    Err(Err::Break(id))
                }),
                id,
            )
        }
        ("limit", 2) => eval(&args[0], env, input.clone(), &mut |n| {
            let n = number_arg(&n, "limit count")?;
            if n == 0.0 {
                return Ok(());
            }
            if n < 0.0 {
                return eval(&args[1], env, input.clone(), out);
            }
            let id = fresh_label();
            let mut count = 0.0;
            break_target(
                eval(&args[1], env, input.clone(), &mut |v| {
                    out(v)?;
                    count += 1.0;
                    if count >= n {
                        Err(Err::Break(id))
                    } else {
                        Ok(())
                    }
                }),
                id,
            )
        }),
        ("range", 2) => eval(&args[0], env, input.clone(), &mut |from| {
            eval(&args[1], env, input.clone(), &mut |upto| {
                let (mut x, end) = (number_arg(&from, "range start")?, number_arg(&upto, "range end")?);
                while x < end {
                    out(Value::Number(x))?;
                    x += 1.0;
                }
                Ok(())
            })
        }),
        ("sort_by" | "group_by" | "unique_by" | "min_by" | "max_by", 1) => {
            let Value::Array(items) = &input else {
                return fail(format!("Cannot index {} with number", input.type_name()));
            };
            let mut keyed = Vec::with_capacity(items.len());
            for x in items.iter() {
                let mut key = Vec::new();
                eval(&args[0], env, x.clone(), &mut |k| {
                    key.push(k);
                    Ok(())
                })?;
                keyed.push((Value::array(key), x.clone()));
            }
            out(by_impl(name, keyed))
        }
        (_, 1) => eval(&args[0], env, input.clone(), &mut |a| out(native1(name, &input, &a)?)),
        (_, 0) => out(native0(name, input)?),
        _ => fail(format!("{}/{} is not defined", name, args.len())),
    }
}

// ---------------------------------------------------------------------------
// Path expressions (path(f), assignment operators, del)

fn invalid_path(ast: &Ast, env: &Rc<Env>, cur: Value) -> R {
    eval(ast, env, cur, &mut |v| fail(format!("Invalid path expression with result {}", to_json(&v, false))))
}

fn with_key(path: &[Value], k: Value) -> Vec<Value> {
    let mut p = path.to_vec();
    p.push(k);
    p
}

fn recurse_paths(path: Vec<Value>, cur: Value, out: PathOut) -> R {
    out(path.clone(), cur.clone())?;
    match &cur {
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                recurse_paths(with_key(&path, Value::Number(i as f64)), x.clone(), out)?;
            }
            Ok(())
        }
        Value::Object(m) => {
            for (k, x) in m.iter() {
                recurse_paths(with_key(&path, Value::string(k)), x.clone(), out)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

pub fn eval_paths(ast: &Ast, env: &Rc<Env>, path: Vec<Value>, cur: Value, out: PathOut) -> R {
    match ast {
        Ast::Identity => out(path, cur),
        Ast::Recurse => recurse_paths(path, cur, out),
        Ast::Index(t, k) => eval(k, env, cur.clone(), &mut |kv| {
            eval_paths(t, env, path.clone(), cur.clone(), &mut |tp, tv| {
                let nv = index(&tv, &kv)?;
                out(with_key(&tp, kv.clone()), nv)
            })
        }),
        Ast::Iterate(t) => eval_paths(t, env, path, cur, &mut |tp, tv| match &tv {
            Value::Array(a) => {
                for (i, x) in a.iter().enumerate() {
                    out(with_key(&tp, Value::Number(i as f64)), x.clone())?;
                }
                Ok(())
            }
            Value::Object(m) => {
                for (k, x) in m.iter() {
                    out(with_key(&tp, Value::string(k)), x.clone())?;
                }
                Ok(())
            }
            _ => fail(cannot_iterate(&tv)),
        }),
        Ast::Pipe(a, b) => eval_paths(a, env, path, cur, &mut |p, v| eval_paths(b, env, p, v, out)),
        Ast::Comma(a, b) => {
            eval_paths(a, env, path.clone(), cur.clone(), out)?;
            eval_paths(b, env, path, cur, out)
        }
        Ast::If(c, t, e) => eval(c, env, cur.clone(), &mut |cv| {
            if cv.truthy() {
                eval_paths(t, env, path.clone(), cur.clone(), out)
            } else {
                match e {
                    Some(e) => eval_paths(e, env, path.clone(), cur.clone(), out),
                    None => out(path.clone(), cur.clone()),
                }
            }
        }),
        Ast::Alt(a, b) => {
            let mut any = false;
            let (r, down) = guarded_paths(out, |sink| {
                eval_paths(a, env, path.clone(), cur.clone(), &mut |p, v| {
                    if v.truthy() {
                        any = true;
                        sink(p, v)
                    } else {
                        Ok(())
                    }
                })
            });
            if let Some(e) = down {
                return Err(e);
            }
            if let Err(Err::Break(id)) = r {
                return Err(Err::Break(id));
            }
            if any {
                Ok(())
            } else {
                eval_paths(b, env, path, cur, out)
            }
        }
        Ast::Try(body, None) => {
            let (r, down) = guarded_paths(out, |sink| eval_paths(body, env, path.clone(), cur.clone(), sink));
            if let Some(e) = down {
                return Err(e);
            }
            match r {
                Err(Err::Val(_)) => Ok(()),
                other => other,
            }
        }
        Ast::As(src, var, body) => eval(src, env, cur.clone(), &mut |v| {
            let inner = bind_var(env, var, v);
            eval_paths(body, &inner, path.clone(), cur.clone(), out)
        }),
        Ast::Def(fd, rest) => eval_paths(rest, &push(env, Bind::Func(fd.clone())), path, cur, out),
        Ast::Call(name, args) => match lookup_func(env, name, args.len()) {
            Some(Found::Def(fd, def_env)) => bind_params(&fd, def_env, args, env, 0, &cur, &mut |e| {
                eval_paths(&fd.body, &e, path.clone(), cur.clone(), out)
            }),
            Some(Found::Param(body, cenv)) => eval_paths(&body, &cenv, path, cur, out),
            None => match (name.as_str(), args.len()) {
                ("empty", 0) => Ok(()),
                ("error", 0) => Err(Err::Val(cur)),
                ("error", 1) => eval(&args[0], env, cur, &mut |m| Err(Err::Val(m))),
                ("getpath", 1) => eval(&args[0], env, cur.clone(), &mut |p| {
                    let extra = as_path(&p)?;
                    let v = getpath(&cur, &extra)?;
                    let mut full = path.clone();
                    full.extend(extra);
                    out(full, v)
                }),
                ("first", 1) => {
                    let id = fresh_label();
                    break_target(
                        eval_paths(&args[0], env, path, cur, &mut |p, v| {
                            out(p, v)?;
                            Err(Err::Break(id))
                        }),
                        id,
                    )
                }
                _ => invalid_path(ast, env, cur),
            },
        },
        _ => invalid_path(ast, env, cur),
    }
}

fn collect_paths(lhs: &Ast, env: &Rc<Env>, input: &Value) -> Result<Vec<Vec<Value>>, Err> {
    let mut paths = Vec::new();
    eval_paths(lhs, env, Vec::new(), input.clone(), &mut |p, _| {
        paths.push(p);
        Ok(())
    })?;
    Ok(paths)
}

fn first_output(ast: &Ast, env: &Rc<Env>, input: Value) -> Result<Option<Value>, Err> {
    let id = fresh_label();
    let mut got = None;
    break_target(
        eval(ast, env, input, &mut |v| {
            got = Some(v);
            Err(Err::Break(id))
        }),
        id,
    )?;
    Ok(got)
}

fn assign(op: AssignOp, lhs: &Ast, rhs: &Ast, env: &Rc<Env>, input: Value, out: Out) -> R {
    match op {
        AssignOp::Update => {
            let mut acc = input.clone();
            for p in collect_paths(lhs, env, &input)? {
                let old = getpath(&acc, &p)?;
                acc = match first_output(rhs, env, old)? {
                    Some(nv) => setpath(acc, &p, nv)?,
                    None => delpaths(acc, &Value::array(vec![Value::array(p)]))?,
                };
            }
            out(acc)
        }
        _ => eval(rhs, env, input.clone(), &mut |rv| {
            let mut acc = input.clone();
            for p in collect_paths(lhs, env, &input)? {
                let nv = match op {
                    AssignOp::Set => rv.clone(),
                    AssignOp::Arith(bop) => binop(bop, getpath(&acc, &p)?, rv.clone())?,
                    _ => {
                        let old = getpath(&acc, &p)?;
                        if old.truthy() {
                            old
                        } else {
                            rv.clone()
                        }
                    }
                };
                acc = setpath(acc, &p, nv)?;
            }
            out(acc)
        }),
    }
}
