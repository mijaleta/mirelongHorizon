# jqlite: a jq-compatible JSON processor in Rust

Implement `jqlite`, a command-line JSON processor that runs programs written in
the [jq](https://jqlang.github.io/jq/) language. It must behave like jq 1.7 for
the subset of the language described below.

The Cargo package in `/app` (package and binary name `jqlite`) currently only
prints "not implemented". Replace it with a full implementation.

## Constraints

- Rust, standard library only. The environment has no network access and no
  crates are vendored, so `[dependencies]` must stay empty.
- jq is not installed, and your program must not run other programs.
- Your crate is graded by copying `/app` (without `target/` and `.git/`) to a
  fresh directory and running `cargo build --release --offline` there with the
  installed Rust 1.83 toolchain. The binary used is `target/release/jqlite`.

## How you are scored

A hidden suite of a few hundred cases is run against the binary. Each case is
a list of command-line arguments, some text on stdin, the expected stdout and
the expected exit status. A case passes only when **stdout matches byte for
byte and the exit status matches**. stderr is never compared. Each case has a
10 second limit. Your score is the fraction of cases that pass.

The hidden cases cover everything in this document: output formatting,
operators, generators and control flow, user-defined functions, string and
collection builtins, path expressions and assignment, the CLI options and
error handling. Their expected outputs were produced by jq 1.7.

`/app/examples/cases.jsonl` holds 41 public cases in the same format, and
`python3 /app/run_examples.py [-v]` builds your crate and runs them. The
hidden cases are different and much more thorough, so test well beyond the
examples.

## Command line

```
jqlite [OPTIONS] FILTER
```

The first argument that is not an option is the jq program. Options can come
before or after it:

| Option | Meaning |
|---|---|
| `-c`, `--compact-output` | Print each output on one line with no extra whitespace. |
| `-r`, `--raw-output` | If an output is a string, print its contents without quotes or escaping. |
| `-n`, `--null-input` | Ignore stdin and run the program once with `null` as input. |
| `-s`, `--slurp` | Read every input value into one array and run the program once on it. |
| `--arg NAME VALUE` | Bind `$NAME` to the string VALUE. |
| `--argjson NAME TEXT` | Bind `$NAME` to the JSON value TEXT. |

Short options can be combined (`-rc`, `-nc`). An unknown option, a missing
program or an extra positional argument is a usage error.

## Input and output

stdin holds zero or more JSON texts separated by optional whitespace (for
example `1 [2] {"a":3}`). The program runs once per value, in order. Strings
can contain `\uXXXX` escapes including surrogate pairs.

Every output value is printed followed by a newline. The default is pretty
printing with two-space indentation:

```
{
  "a": [
    1,
    {}
  ],
  "b": "x"
}
```

Empty arrays and objects print as `[]` and `{}`. With `-c` the same value is
`{"a":[1,{}],"b":"x"}`. Object keys keep their insertion order.

Strings are printed as JSON. `"` and `\` are escaped, as are newline, tab,
carriage return, backspace and form feed (`\n \t \r \b \f`). Other characters
below U+0020 and U+007F print as `\u00XX` with lowercase hex. All other
characters, including non-ASCII, print as raw UTF-8.

### Numbers

Numbers are IEEE-754 doubles. To print one, take the shortest digit string
`d1 d2 ... dn` that round-trips, and the decimal exponent `decpt` such that the
value is `0.d1d2...dn × 10^decpt`. If `decpt <= -4` or `decpt > n + 15`, print
in exponent form `d1[.d2...dn]e±XX`, with at least two exponent digits.
Otherwise print positionally, padding with zeros as needed. Negative zero
prints as `-0`. Examples:

| value | printed |
|---|---|
| 1500 | `1500` |
| 0.001 | `0.001` |
| 0.00001 | `1e-05` |
| 1e15 | `1000000000000000` |
| 1e16 | `1e+16` |
| 2/3 × 1e20 | `66666666666666660000` |
| 0.1 × 3 | `0.30000000000000004` |
| 1e300 × 10 | `1e+301` |

(jq 1.7 sometimes echoes an input number literal exactly as written. The test
inputs avoid such literals, so always print the canonical form above.)

## The language

### Lexical structure

- Comments run from `#` to the end of the line.
- Number literals: `12`, `1.5`, `1e3`, `2.5e-3`.
- String literals use JSON escapes plus interpolation: `"a\(expr)b"`.
- Identifiers: `[A-Za-z_][A-Za-z0-9_]*`. Variables: `$name`.
- Keywords: `def if then elif else end as reduce foreach try catch and or`,
  plus the literals `true false null`.

### Expressions (highest precedence first)

1. **Terms.**
   - `.` (identity) and `..` (recursive descent: the value, then all its
     descendants, pre-order).
   - Field access `.foo`, `."foo"`, `.["foo"]`; index `.[e]`; iteration
     `.[]`; slices `.[e:e]`, `.[e:]`, `.[:e]`.
   - Literals, `$var`, `(e)`, array construction `[e]` / `[]`, object
     construction, `if`, `try`, `reduce`, `foreach`, function calls
     `f` / `f(a; b)`.
   - Postfix suffixes chain on any term: `.a.b[0]["c"][]`, `$x[1:]`,
     `(e).k`. A postfix `?` (as in `.a?` or `.[]?`) means `try` without
     `catch`.
2. Unary minus: `-e`.
3. `*` `/` `%` (left associative).
4. `+` `-` (left associative).
5. `==` `!=` `<` `<=` `>` `>=` (non-associative).
6. `and`
7. `or`
8. Assignments `=` `|=` `+=` `-=` `*=` `/=` `%=` `//=` (non-associative).
9. `//` (right associative).
10. `,`
11. `|` (right associative). The lowest-precedence forms also live at this
    level: `TERM as $x | BODY` and `def f: BODY; REST`.

`try BODY catch HANDLER`: BODY and HANDLER are single postfix terms, so use
parentheses for anything larger, for example `try (1/0) catch "div"`.

Object construction: `{a: e}`, `{"a b": e}`, `{"\(k)": e}`, `{(e): e}`,
`{a}` (meaning `{a: .a}`), `{"a"}` and `{$x}` (meaning `{x: $x}`). Keys may
be keywords. A value is an expression without a top-level `,` (pipes are
allowed). A computed key that is not a string is an error. A repeated key
keeps the position of its first occurrence and takes the last value.

### Generators and evaluation order

Every expression produces zero or more outputs. When an expression combines
several generators, the outputs form a cartesian product in this exact order:

- `A op B` for arithmetic and comparison operators: for each output of `B`
  (outer loop), for each output of `A` (inner loop). So
  `[(1,2) + (10,20)]` is `[11,12,21,22]`.
- `A and B`, `A or B`: for each output of `A` (outer). If it decides the
  result (falsy for `and`, truthy for `or`), output that boolean without
  evaluating `B`. Otherwise output the truthiness of each output of `B`.
- `T[K]`: for each output of `K` (outer), for each output of `T`. `K` is
  evaluated against the same input as `T`, so `.a[.i]` indexes `.a` with
  `.i`. In a slice `T[F:E]`, `F` is the outermost loop, then `E`, then `T`.
- Object construction: entries left to right, key before value, with earlier
  entries varying slowest. `{a:(1,2), b:(3,4)}` gives `{"a":1,"b":3}`,
  `{"a":1,"b":4}`, `{"a":2,"b":3}`, `{"a":2,"b":4}`.
- String interpolation: the **last** interpolation varies slowest.
  `"\(1,2)-\(3,4)"` gives `"1-3"`, `"2-3"`, `"1-4"`, `"2-4"`.
- `A , B`: all outputs of `A`, then all outputs of `B`. `A | B`: `B` is run
  on each output of `A`.
- `[e]` collects every output of `e` into one array.

### Values, truthiness and ordering

`false` and `null` are falsy. Everything else is truthy, including `0`,
`""` and `[]`.

Total order, used by comparisons and sorting: `null < false < true <
numbers < strings < arrays < objects`. Numbers compare numerically. Strings
compare by their UTF-8 bytes. Arrays compare lexicographically, and a proper
prefix is smaller. Objects compare first by their sorted key lists (as
arrays), then by the values of those keys in sorted-key order. Equality is
this order returning equal, so `1 == 1.0` is true.

### Operators

- `+`: `null + x` and `x + null` are `x`. Numbers add. Strings and arrays
  concatenate. Objects merge shallowly: keys from the right win, existing keys
  keep their position and new keys are appended.
- `-`: numbers subtract. `array - array` removes every element equal to any
  element of the right side.
- `*`: numbers multiply. `string * n` and `n * string` repeat the string
  `floor(n)` times, giving `""` for `0 <= n < 1` and `null` for `n < 0`.
  `object * object` merges recursively: when both sides hold an object under
  the same key, those are merged too.
- `/`: numbers divide, and dividing by zero is an error. `string / string`
  splits, like `split`.
- `%`: both operands are truncated to integers. The result has the sign of the
  dividend (`-17 % 5` is `-2`, `5.9 % 2.1` is `1`). Modulo by zero is an
  error.
- Any other combination of types is an error.
- `A // B`: outputs every output of `A` that is truthy. Errors raised while
  evaluating `A` are suppressed. If `A` produced no truthy output, outputs the
  outputs of `B` instead.
- `-e` negates numbers. Anything else is an error.

### Indexing, slicing and iteration

- `.[k]` on an object needs a string key and gives `null` if it is missing.
  On an array it needs a number: the number is floored, negative indices count
  from the end, and out-of-range gives `null`. On `null` any string or number
  index gives `null`. Anything else is an error.
- Slices work on arrays and strings (by Unicode code points), and on `null`
  (giving `null`). A `null` bound means the start or the end. Negative bounds
  count from the end. Bounds are clamped to the length, the start is floored,
  the end is rounded up, and an end before the start gives an empty result.
- `.[]` outputs the elements of an array or the values of an object in order.
  On anything else it is an error, `null` included.

### Variables, reduce and foreach

- `TERM as $x | BODY` runs BODY once for each output of TERM, with `$x`
  bound. BODY's input is the original input. Bindings are lexically scoped.
- `reduce SRC as $x (INIT; UPDATE)`: for each output of INIT, start a state
  equal to it. For each output `$x` of SRC (evaluated on the original input),
  the state becomes the **last** output of UPDATE run on the current state.
  Output the final state.
- `foreach SRC as $x (INIT; UPDATE; EXTRACT)`: like reduce, but every output
  of UPDATE becomes the new state, and EXTRACT runs on it with `$x` bound and
  its outputs are emitted. Without EXTRACT the states themselves are emitted.

### Conditionals and errors

- `if C then A elif C2 then B else D end`: for each output of C, run A if it
  is truthy, otherwise continue with the `elif`/`else` branches. A missing
  `else` acts as `else . end`.
- `error(msg)` raises an error carrying the value `msg` (any JSON value).
  `error` (no argument) uses its input as the value.
- `try BODY catch HANDLER`: BODY's outputs pass through until BODY raises an
  error. Evaluation of BODY then stops, and HANDLER runs with the error value
  as its input. `try BODY` and `BODY?` just stop silently. **Only errors raised
  inside BODY are caught.** An error raised later by whatever consumes BODY's
  outputs propagates normally.
- Built-in errors carry a message string. The text of those messages is never
  tested.

### User-defined functions

`def name: BODY;` and `def name(f; g; $v): BODY;`. A definition is visible in
the expression that follows it, in its own body (so recursion works) and in
later nested definitions. Functions are identified by name and arity.
A later definition shadows earlier ones and built-ins.

- A filter parameter `f` is a closure: when the body calls `f`, the argument
  expression is evaluated in the caller's scope against whatever input the
  body passes to `f`.
- A value parameter `$v` is evaluated against the function's input. The body
  runs once per output, earlier parameters varying slowest. Inside the body
  both `$v` and the filter `v` are available.
- Recursion must work at least 3000 calls deep.

### Path expressions and assignment

Some expressions can be evaluated as **paths** (arrays of keys and indices
that locate values inside the input). These are `.`, `..`, `.foo`, `.[e]`,
`.[]`, `|`, `,`, `if`, `//`, `?` / `try` without `catch`, `TERM as $x |
BODY`, `empty`, `error`, `select(f)`, `recurse`, `first(f)`, `getpath(p)`, and
user functions whose bodies are path expressions. Any other expression used
where a path is required is an error.

- `path(f)` outputs the path of each output of `f`.
- `LHS = RHS`: for each output `$v` of RHS (evaluated on the input), output
  the input with every path of LHS set to `$v`.
- `LHS |= F`: for each path `p` of LHS, in order, replace the value at `p`
  with the first output of `F` run on it. If `F` produces nothing, delete the
  path. Outputs one result.
- `LHS op= RHS` for `+ - * / %`: for each output `$v` of RHS (evaluated on the
  input), update every path `p` of LHS to `getpath(p) op $v`.
- `LHS //= RHS`: the same, with `getpath(p) // $v`.
- Setting a path creates missing objects and arrays (`null` becomes `{}` or
  `[]`). Arrays are padded with `null` when a later index is set.
- All paths are computed from the original input before any update is applied.

## Built-in functions

Functions not listed here do not need to exist.

**Core:** `empty`, `error`, `error(msg)`, `not`, `select(f)`, `type` (`"null"`,
`"boolean"`, `"number"`, `"string"`, `"array"`, `"object"`), `length` (null 0,
number its absolute value, string its code point count, array or object its
size, boolean an error), `utf8bytelength`, `values` (non-null), `nulls`,
`booleans`, `numbers`, `strings`, `arrays`, `objects`, `iterables`, `scalars`.

**Generators:** `range(n)`, `range(from; upto)` (from, from+1, … while
< upto), `recurse` (same as `..`), `recurse(f)` (`def recurse(f): def r: .,
(f | r); r;`), `limit(n; f)` (the first n outputs; nothing if n is 0; all
outputs if n is negative), `first(f)`, `last(f)`, `isempty(g)`,
`while(cond; update)`, `until(cond; update)`. These must be lazy:
`limit(3; recurse(. + 1))` and `first(range(10; 100000000000))` must finish
instantly.

**Arrays and objects:** `keys` (sorted object keys, or array indices),
`keys_unsorted` (insertion order), `has(k)` (object key, or `0 <= k <
length` for arrays), `in(x)`, `contains(x)`, `inside(x)`, `map(f)`,
`map_values(f)` (`.[] |= f`), `add` (folds `+` starting from `null`; an empty
array gives `null`), `any`, `all`, `any(f)`, `all(f)`, `first`, `last`,
`nth(n)` (`.[0]`, `.[-1]`, `.[n]`), `flatten`, `flatten(depth)` (negative
depth is an error), `reverse` (arrays, and `null` gives `[]`), `sort`,
`sort_by(f)`, `group_by(f)`, `unique`, `unique_by(f)`, `min`, `max`,
`min_by(f)`, `max_by(f)`, `to_entries`, `from_entries`, `with_entries(f)`,
`transpose`, `walk(f)`.

- `contains`: strings use substring containment. For arrays, every element of
  the argument must be contained in some element of the input. For objects,
  every key of the argument must exist in the input with a value that
  contains it. Other values use equality. Inputs of different types are an
  error.
- `sort_by(f)` and friends use the array of **all** outputs of `f` as the
  key, compare keys with the total order above and sort stably. `group_by`
  returns the groups in key order. `unique_by` keeps the first element of each
  group. `unique` is `unique_by(.)`. `min_by` returns the first element with
  the smallest key and `max_by` the **last** element with the largest key.
  `min`, `max` and these functions return `null` for an empty array.
- `to_entries` gives `[{"key": k, "value": v}, …]` in insertion order.
  `from_entries` takes the key from `.key`, or `.name` when `.key` is `null`
  or `false`, and the value from `.value` (`null` when missing). Later entries
  overwrite earlier ones.
- `walk(f)`: `def walk(f): def w: if type == "object" then map_values(w)
  elif type == "array" then map(w) else . end | f; w;`
- `transpose` pads shorter rows with `null`.

**Strings:** `tostring` (strings are unchanged, anything else becomes its
compact JSON), `tojson`, `fromjson`, `tonumber` (number input unchanged, or a
string holding a JSON number), `ascii_downcase`, `ascii_upcase` (ASCII
letters only), `explode`, `implode` (code points), `ltrimstr(s)`,
`rtrimstr(s)` (input unchanged if it is not a string or does not have the
prefix or suffix), `startswith(s)`, `endswith(s)`, `split(s)` (an empty input
string gives `[]`, and an empty separator splits into characters), and
`join(s)`:

```
def join($x): reduce .[] as $i (null;
    (if . == null then "" else . + $x end)
    + ($i | if . == null then "" elif type == "string" then . else tojson end)) // "";
```

**Numbers:** `floor`, `ceil`, `round` (halves away from zero), `sqrt`.

**Paths:** `path(f)`, `paths` (`path(..) | select(length > 0)`),
`paths(f)` (the paths whose value satisfies `f`), `getpath(p)` (`null` where
the path is missing), `setpath(p; v)`, `delpaths(ps)` (deletes all paths,
longest and highest index first, so earlier deletions do not shift later
ones), `del(f)` (`delpaths([path(f)])`), `to_entries`.

## Errors and exit status

- **Compile errors** (syntax errors, calls to undefined functions or wrong
  arities, undefined variables): print nothing to stdout and exit with
  status 3.
- **Runtime errors**: when an uncaught error is raised while processing an
  input, outputs already printed for that input stay printed, a message goes
  to stderr, and processing continues with the next input. Like jq, **the exit
  status reflects only the last input**: 5 if processing the last input raised
  an uncaught error, otherwise 0.
- **Invalid JSON on stdin**: every value before the bad text is processed
  normally, then a message goes to stderr and jqlite exits with status 5. With
  `-s` nothing is printed.
- **Usage errors** (bad options, missing program, invalid `--argjson` text):
  exit with status 2.
- Otherwise exit with status 0.

## Out of scope

`$ENV`/`env`, `input`/`inputs`, `@format` strings, regular expressions,
`label`/`break`, destructuring patterns, `?//`, modules, `limit` on paths,
`reduce`/`foreach` as path expressions, slice assignment, `getpath` on
invalid paths, SQL-style and date builtins, `--tab`/`--indent`/`-S` and other
options not listed above.
