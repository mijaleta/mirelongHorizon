mod interp;
mod parser;
mod prelude;
mod value;

use std::io::{Read, Write};
use value::Value;

struct Options {
    compact: bool,
    raw: bool,
    null_input: bool,
    slurp: bool,
}

fn main() {
    // Deeply recursive jq programs need far more stack than the default main thread has.
    let worker = std::thread::Builder::new().stack_size(1 << 30).spawn(run).expect("failed to start interpreter");
    let code = worker.join().unwrap_or(5);
    std::process::exit(code);
}

fn usage() -> i32 {
    eprintln!("Usage: jqlite [-c] [-r] [-n] [-s] [--arg NAME VALUE] [--argjson NAME JSON] FILTER");
    2
}

fn emit(v: &Value, opts: &Options, w: &mut impl Write) {
    let text = match v {
        Value::String(s) if opts.raw => s.to_string(),
        _ => value::to_json(v, !opts.compact),
    };
    let _ = w.write_all(text.as_bytes());
    let _ = w.write_all(b"\n");
}

/// Runs the program on one input; returns false if it raised an error.
fn process(program: &parser::P, env: &std::rc::Rc<interp::Env>, input: Value, opts: &Options, w: &mut impl Write) -> bool {
    let result = interp::eval(program, env, input, &mut |v| {
        emit(&v, opts, w);
        Ok(())
    });
    match result {
        Ok(()) => true,
        Err(interp::Err::Val(e)) => {
            let _ = w.flush();
            let msg = match &e {
                Value::String(s) => s.to_string(),
                other => format!("{} (not a string)", value::to_json(other, false)),
            };
            eprintln!("jqlite: error: {}", msg);
            false
        }
        Err(interp::Err::Break(_)) => {
            eprintln!("jqlite: error: break escaped its label");
            false
        }
    }
}

fn run() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut opts = Options { compact: false, raw: false, null_input: false, slurp: false };
    let mut named: Vec<(String, Value)> = Vec::new();
    let mut filter: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--arg" | "--argjson" => {
                if i + 2 >= args.len() {
                    return usage();
                }
                let v = if a == "--arg" {
                    Value::string(&args[i + 2])
                } else {
                    match value::parse_single(&args[i + 2]) {
                        Ok(v) => v,
                        Err(e) => {
                            eprintln!("jqlite: invalid JSON text passed to --argjson: {}", e);
                            return 2;
                        }
                    }
                };
                named.push((args[i + 1].clone(), v));
                i += 3;
                continue;
            }
            "--compact-output" => opts.compact = true,
            "--raw-output" => opts.raw = true,
            "--null-input" => opts.null_input = true,
            "--slurp" => opts.slurp = true,
            _ if a.starts_with("--") => return usage(),
            _ if a.len() > 1 && a.starts_with('-') => {
                for ch in a[1..].chars() {
                    match ch {
                        'c' => opts.compact = true,
                        'r' => opts.raw = true,
                        'n' => opts.null_input = true,
                        's' => opts.slurp = true,
                        _ => return usage(),
                    }
                }
            }
            _ if filter.is_none() => filter = Some(a.to_string()),
            _ => return usage(),
        }
        i += 1;
    }
    let Some(filter) = filter else {
        return usage();
    };

    let names: Vec<String> = named.iter().map(|(n, _)| n.clone()).collect();
    let program = match interp::compile(&filter, &names) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("jqlite: error: {}\njqlite: 1 compile error", e);
            return 3;
        }
    };
    let mut env = interp::Env::root();
    for (n, v) in named {
        env = interp::bind_var(&env, &n, v);
    }

    let stdout = std::io::stdout();
    let mut w = std::io::BufWriter::new(stdout.lock());
    let mut status = 0;

    if opts.null_input {
        if !process(&program, &env, Value::Null, &opts, &mut w) {
            status = 5;
        }
        let _ = w.flush();
        return status;
    }

    let mut raw = Vec::new();
    if std::io::stdin().read_to_end(&mut raw).is_err() {
        eprintln!("jqlite: error: could not read standard input");
        return 2;
    }
    let text = String::from_utf8_lossy(&raw);
    let mut reader = value::JsonReader::new(&text);

    if opts.slurp {
        let mut all = Vec::new();
        while let Some(next) = reader.next_value() {
            match next {
                Ok(v) => all.push(v),
                Err(e) => {
                    eprintln!("jqlite: error: {}", e);
                    return 5;
                }
            }
        }
        if !process(&program, &env, Value::array(all), &opts, &mut w) {
            status = 5;
        }
        let _ = w.flush();
        return status;
    }

    // Like jq, the exit status reflects only the last input processed.
    while let Some(next) = reader.next_value() {
        match next {
            Ok(v) => status = if process(&program, &env, v, &opts, &mut w) { 0 } else { 5 },
            Err(e) => {
                let _ = w.flush();
                eprintln!("jqlite: error: {}", e);
                return 5;
            }
        }
    }
    let _ = w.flush();
    status
}
