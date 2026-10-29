//! Builtins that are defined in the jq language itself.

pub const PRELUDE: &str = r#"
def not: if . then false else true end;
def select(f): if f then . else empty end;
def recurse(f): def r: ., (f | r); r;
def recurse: recurse(.[]?);
def map(f): [.[] | f];
def map_values(f): .[] |= f;
def add: reduce .[] as $x (null; . + $x);
def any: reduce .[] as $x (false; . or $x);
def all: reduce .[] as $x (true; . and $x);
def any(f): reduce (.[] | f) as $x (false; . or $x);
def all(f): reduce (.[] | f) as $x (true; . and $x);
def range($n): range(0; $n);
def first: .[0];
def last: .[-1];
def last(f): reduce f as $x (null; $x);
def nth($n): .[$n];
def isempty(g): first((g | false), true);
def while(cond; update): def _while: if cond then ., (update | _while) else empty end; _while;
def until(cond; update): def _until: if cond then . else (update | _until) end; _until;
def values: select(. != null);
def nulls: select(. == null);
def booleans: select(type == "boolean");
def numbers: select(type == "number");
def strings: select(type == "string");
def arrays: select(type == "array");
def objects: select(type == "object");
def iterables: select(type == "array" or type == "object");
def scalars: select(type != "array" and type != "object");
def to_entries: [keys_unsorted[] as $k | {key: $k, value: .[$k]}];
def from_entries: reduce .[] as $x ({}; . + { ($x | .key // .name): ($x | .value) });
def with_entries(f): to_entries | map(f) | from_entries;
def join($x): reduce .[] as $i (null;
    (if . == null then "" else . + $x end)
    + ($i | if . == null then "" elif type == "string" then . else tojson end)) // "";
def del(f): delpaths([path(f)]);
def paths: path(..) | select(length > 0);
def paths(node_filter): . as $dot | paths | select(. as $p | $dot | getpath($p) | node_filter);
def walk(f): def w: if type == "object" then map_values(w) elif type == "array" then map(w) else . end | f; w;
def in(xs): . as $x | xs | has($x);
def inside(xs): . as $x | xs | contains($x);
def transpose: if . == [] then [] else . as $in | (map(length) | max) as $max
    | [range(0; $max) as $j | [range(0; $in | length) as $i | $in[$i][$j]]] end;
"#;
