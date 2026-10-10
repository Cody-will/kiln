//! Unit tests for the type checker.

use crate::check_source;

fn errs(src: &str) -> Vec<String> {
    check_source(src).into_iter().map(|e| e.message).collect()
}

#[track_caller]
fn ok(src: &str) {
    let e = errs(src);
    assert!(e.is_empty(), "unexpected errors: {e:#?}");
}

/// Asserts exactly one error containing `needle`.
#[track_caller]
fn err1(src: &str, needle: &str) {
    let e = errs(src);
    assert!(
        e.len() == 1 && e[0].contains(needle),
        "expected one error containing {needle:?}, got {e:#?}"
    );
}

/// Asserts at least one error containing `needle`.
#[track_caller]
fn has(src: &str, needle: &str) {
    let e = errs(src);
    assert!(
        e.iter().any(|m| m.contains(needle)),
        "expected an error containing {needle:?}, got {e:#?}"
    );
}

// ----- basics and inference ---------------------------------------------------------

#[test]
fn hello_world() {
    ok("fn main() {\n println(\"hi\")\n}");
}

#[test]
fn literal_inference_and_annotations() {
    ok("fn main() {\n let a: u8 = 3\n b := 2.5\n let c: f32 = b\n d := a + 1\n let e: u8 = d\n}");
}

#[test]
fn mismatched_let_annotation() {
    err1(
        "fn main() {\n let a: bool = 3\n}",
        "expected `bool`, found `{integer}`",
    );
}

#[test]
fn int_and_float_do_not_mix() {
    err1(
        "fn main() {\n x := 1 + 2.0\n}",
        "cannot apply `+` to `{integer}` and `{float}`",
    );
}

#[test]
fn casts() {
    ok("fn main() {\n x := 3\n y := x as f64\n c := 65u8 as char\n n := 'a' as u32\n println(\"{y}{c}{n}\")\n}");
    err1(
        "fn main() {\n b := true as f64\n}",
        "invalid cast from `bool` to `f64`",
    );
}

#[test]
fn string_interpolation_is_string() {
    ok("fn f() -> String {\n x := 1\n return \"{x}\"\n}\nfn main() {}");
}

#[test]
fn string_concat_and_deref_coercion() {
    ok("fn takes(s: &str) -> usize {\n return s.len()\n}\nfn main() {\n s := String::from(\"a\") + \"b\"\n n := takes(&s)\n println(\"{n}\")\n}");
    has(
        "fn takes(s: &str) {}\nfn main() {\n s := String::from(\"a\")\n takes(s)\n}",
        "expected `&str`, found `String`",
    );
}

#[test]
fn unresolved_type_needs_annotation() {
    err1(
        "fn main() {\n v := Vec::new()\n}",
        "type annotations needed for `v`",
    );
    ok("fn main() {\n v mut := Vec::new()\n v.push(1)\n}");
    ok("fn main() {\n let v: Vec<u8> = Vec::new()\n}");
}

// ----- explicit return (SPEC §3.1) ---------------------------------------------------

#[test]
fn explicit_return_required() {
    err1("fn f() -> i32 {\n 1\n}\nfn main() {}", "missing `return`");
}

#[test]
fn missing_return_on_some_path() {
    err1(
        "fn f(b: bool) -> i32 {\n if b {\n return 1\n }\n}\nfn main() {}",
        "must return a value of type `i32` on every path",
    );
}

#[test]
fn all_paths_returning_is_fine() {
    ok("fn f(b: bool) -> i32 {\n if b {\n return 1\n } else {\n return 2\n }\n}\nfn main() {}");
    ok("fn f(x: i32) -> i32 {\n match x {\n 0 => return 1,\n _ => return 2,\n }\n}\nfn main() {}");
    ok("fn f() -> i32 {\n loop {\n return 3\n }\n}\nfn main() {}");
    ok("fn f() -> i32 {\n panic(\"no\")\n}\nfn main() {}");
}

#[test]
fn unit_function_returning_value() {
    err1(
        "fn f() {\n return 5\n}\nfn main() {}",
        "this function returns `()`",
    );
}

#[test]
fn wrong_return_type() {
    err1(
        "fn f() -> bool {\n return 5\n}\nfn main() {}",
        "expected `bool`, found `{integer}`",
    );
    err1(
        "fn f() -> i32 {\n return\n}\nfn main() {}",
        "`return` without a value",
    );
}

#[test]
fn if_and_match_as_values() {
    ok("fn f(b: bool) -> i32 {\n x := if b { 1 } else { 2 }\n y := match x {\n 1 => 10,\n _ => 20,\n }\n return y\n}\nfn main() {}");
    has(
        "fn f(b: bool) {\n x := if b { 1 } else { \"no\" }\n}\nfn main() {}",
        "incompatible types",
    );
    has(
        "fn f(b: bool) {\n let x: i32 = if b { 1 }\n}\nfn main() {}",
        "`if` without `else`",
    );
}

#[test]
fn loop_break_value() {
    ok("fn main() {\n i mut := 0\n x := loop {\n i += 1\n if i > 3 {\n break i * 2\n }\n }\n let y: i64 = x\n}");
    has(
        "fn main() {\n while true {\n break 3\n }\n}",
        "only allowed inside `loop`",
    );
}

// ----- := rules ---------------------------------------------------------------------

#[test]
fn short_decl_tuple_destructuring() {
    ok("fn pair() -> (i32, bool) {\n return (1, true)\n}\nfn main() {\n (a, b) := pair()\n let x: bool = b\n let y: i32 = a\n}");
    err1("fn main() {\n (a, b) := 5\n}", "cannot destructure");
}

#[test]
fn short_decl_of_unit_is_an_error() {
    err1("fn f() {}\nfn main() {\n x := f()\n}", "type `()`");
}

#[test]
fn immutable_assignment() {
    err1(
        "fn main() {\n x := 1\n x = 2\n}",
        "cannot assign twice to immutable variable `x`",
    );
    ok("fn main() {\n x mut := 1\n x = 2\n x += 1\n}");
    ok("fn main() {\n let mut x = 1\n x = 2\n}");
}

#[test]
fn mutable_borrow_requires_mut_binding() {
    err1(
        "fn main() {\n v := Vec::<i32>::new()\n r := &mut v\n}",
        "cannot borrow `v`",
    );
}

#[test]
fn mut_self_method_requires_mut_binding() {
    has(
        "fn main() {\n let v: Vec<i32> = Vec::new()\n v.push(1)\n}",
        "cannot borrow `v`",
    );
}

#[test]
fn assignment_through_shared_ref() {
    has(
        "struct P {\n x: i32,\n}\nfn f(p: &P) {\n p.x = 3\n}\nfn main() {}",
        "behind a `&` reference",
    );
    ok("struct P {\n x: i32,\n}\nfn f(p: &mut P) {\n p.x = 3\n}\nfn main() {}");
}

// ----- structs and enums --------------------------------------------------------------

const SHAPES: &str = "
struct Point {
\tx: f64,
\ty: f64,
}

enum Shape {
\tCircle(f64),
\tRect { w: f64, h: f64 },
\tEmpty,
}

impl Point {
\tfn new(x: f64, y: f64) -> Point {
\t\treturn Point { x: x, y: y }
\t}
\tfn len(&self) -> f64 {
\t\treturn self.x * self.x + self.y * self.y
\t}
\tfn scale(&mut self, k: f64) {
\t\tself.x *= k
\t\tself.y *= k
\t}
}
";

fn with_shapes(body: &str) -> String {
    format!("{SHAPES}\n{body}")
}

#[test]
fn struct_literals_fields_and_methods() {
    ok(&with_shapes("fn main() {\n p mut := Point::new(1.0, 2.0)\n p.scale(2.0)\n let l: f64 = p.len()\n x := p.x\n}"));
}

#[test]
fn struct_literal_errors() {
    has(
        &with_shapes("fn main() {\n p := Point { x: 1.0 }\n}"),
        "missing field `y`",
    );
    has(
        &with_shapes("fn main() {\n p := Point { x: 1.0, y: 2.0, z: 3.0 }\n}"),
        "no field named `z`",
    );
    has(
        &with_shapes("fn main() {\n p := Point { x: 1.0, y: true }\n}"),
        "expected `f64`, found `bool`",
    );
}

#[test]
fn unknown_field_and_method() {
    has(
        &with_shapes("fn main() {\n p := Point::new(1.0, 2.0)\n z := p.z\n}"),
        "no field `z` on type `Point`",
    );
    has(
        &with_shapes("fn main() {\n p := Point::new(1.0, 2.0)\n p.fly()\n}"),
        "no method named `fly`",
    );
}

#[test]
fn wrong_argument_count_and_type() {
    has(
        &with_shapes("fn main() {\n p := Point::new(1.0)\n}"),
        "takes 2 arguments but 1 was supplied",
    );
    has(
        &with_shapes("fn main() {\n p := Point::new(1.0, \"x\")\n}"),
        "expected `f64`, found `&str`",
    );
}

#[test]
fn enum_match_with_ref_ergonomics() {
    ok(&with_shapes(
        "fn area(s: &Shape) -> f64 {\n return match s {\n Shape::Circle(r) => 3.0 * r * r,\n Shape::Rect { w, h } => w * h,\n Shape::Empty => 0.0,\n }\n}\nfn main() {}",
    ));
}

#[test]
fn non_exhaustive_match() {
    err1(
        &with_shapes("fn f(s: Shape) -> i32 {\n match s {\n Shape::Circle(_) => return 1,\n Shape::Empty => return 2,\n }\n}\nfn main() {}"),
        "non-exhaustive match: `Rect` not covered",
    );
    err1(
        "fn f(x: i32) {\n match x {\n 1 => {},\n }\n}\nfn main() {}",
        "not all values of `i32`",
    );
    ok("fn f(b: bool) -> i32 {\n match b {\n true => return 1,\n false => return 0,\n }\n}\nfn main() {}");
}

#[test]
fn guarded_arms_do_not_count() {
    has("fn f(x: Option<i32>) {\n match x {\n Some(v) if v > 0 => {},\n None => {},\n }\n}\nfn main() {}", "`Some` not covered");
}

#[test]
fn pattern_arity_and_fields() {
    has(&with_shapes("fn f(s: Shape) {\n match s {\n Shape::Circle(a, b) => {},\n _ => {},\n }\n}\nfn main() {}"), "this pattern has 2 fields");
    has(&with_shapes("fn f(s: Shape) {\n match s {\n Shape::Rect { w } => {},\n _ => {},\n }\n}\nfn main() {}"), "pattern does not mention field `h`");
    ok(&with_shapes("fn f(s: Shape) {\n match s {\n Shape::Rect { w, .. } => {},\n _ => {},\n }\n}\nfn main() {}"));
}

#[test]
fn generic_enum_and_option() {
    ok("fn first(v: &Vec<i32>) -> Option<i32> {\n if v.is_empty() {\n return None\n }\n return Some(v[0])\n}\nfn main() {\n x := first(&Vec::new()).unwrap_or(0)\n let y: i32 = x\n}");
    has(
        "fn f() -> Option<i32> {\n return Some(true)\n}\nfn main() {}",
        "expected `Option<i32>`, found `Option<bool>`",
    );
}

#[test]
fn generic_struct() {
    ok("struct Pair<T> {\n a: T,\n b: T,\n}\nimpl<T> Pair<T> {\n fn first(&self) -> &T {\n return &self.a\n }\n}\nfn main() {\n p := Pair { a: 1u8, b: 2 }\n let x: &u8 = p.first()\n}");
    has(
        "struct Pair<T> {\n a: T,\n b: T,\n}\nfn main() {\n p := Pair { a: 1, b: true }\n}",
        "expected `{integer}`, found `bool`",
    );
    has(
        "struct W<T> {\n v: T,\n}\nfn f(w: W) {}\nfn main() {}",
        "takes 1 generic argument but 0 were supplied",
    );
}

#[test]
fn tuple_structs_and_index() {
    ok("struct Meters(f64)\nfn main() {\n m := Meters(3.0)\n let x: f64 = m.0\n t := (1, true)\n let b: bool = t.1\n}");
    has("fn main() {\n t := (1, true)\n x := t.2\n}", "no field `2`");
}

// ----- traits and generics ---------------------------------------------------------------

const TRAITS: &str = "
trait Describe {
\tfn describe(&self) -> String
\tfn shout(&self) -> String {
\t\treturn self.describe().to_upper()
\t}
}

struct Dog {
\tname: String,
}

impl Describe for Dog {
\tfn describe(&self) -> String {
\t\treturn \"dog {self.name}\"
\t}
}
";

#[test]
fn trait_impl_and_default_method() {
    ok(&format!("{TRAITS}\nfn main() {{\n d := Dog {{ name: String::from(\"rex\") }}\n s := d.shout()\n t := d.describe()\n}}"));
}

#[test]
fn missing_trait_method() {
    err1("trait T {\n fn a(&self)\n fn b(&self)\n}\nstruct S\nimpl T for S {\n fn a(&self) {}\n}\nfn main() {}", "missing: `b`");
}

#[test]
fn extra_trait_method() {
    err1("trait T {\n fn a(&self)\n}\nstruct S\nimpl T for S {\n fn a(&self) {}\n fn z(&self) {}\n}\nfn main() {}", "`z` is not a member of trait `T`");
}

#[test]
fn incompatible_trait_signature() {
    err1("trait T {\n fn a(&self) -> i32\n}\nstruct S\nimpl T for S {\n fn a(&self) -> bool {\n return true\n }\n}\nfn main() {}", "incompatible signature");
    err1("trait T {\n fn a(&self)\n}\nstruct S\nimpl T for S {\n fn a(&mut self) {}\n}\nfn main() {}", "incompatible signature");
}

#[test]
fn generic_bound_methods() {
    ok(&format!("{TRAITS}\nfn loud<T: Describe>(x: &T) -> String {{\n return x.shout()\n}}\nfn main() {{\n d := Dog {{ name: String::from(\"a\") }}\n s := loud(&d)\n}}"));
    has(
        "fn f<T>(x: T) {\n x.describe()\n}\nfn main() {}",
        "no method named `describe`",
    );
}

#[test]
fn trait_bounds_checked_at_call_sites() {
    let src = "fn biggest<T: Ord>(a: T, b: T) -> T {\n if a > b {\n return a\n }\n return b\n}\nstruct P\n";
    ok(&format!("{src}fn main() {{\n x := biggest(1, 2)\n}}"));
    err1(
        &format!("{src}fn main() {{\n x := biggest(P, P)\n}}"),
        "the trait `Ord` is not implemented for `P`",
    );
}

#[test]
fn derive_satisfies_bounds() {
    ok("#[derive(Eq, Ord)]\nstruct P\nfn big<T: Ord>(a: T) {}\nfn main() {\n big(P)\n}");
}

#[test]
fn comparisons_need_eq() {
    err1(
        "struct P\nfn main() {\n b := P == P\n}",
        "`Eq` is not implemented for `P`",
    );
    ok("#[derive(Eq)]\nstruct P\nfn main() {\n b := P == P\n}");
    ok("fn main() {\n b := 1.0 < 2.0\n c := \"a\" == \"b\"\n}");
}

#[test]
fn generic_inference_across_calls() {
    ok("fn id<T>(x: T) -> T {\n return x\n}\nfn main() {\n let a: u64 = id(3)\n b := id(true)\n let c: bool = b\n}");
    has(
        "fn id<T>(x: T) -> T {\n return x\n}\nfn main() {\n let a: bool = id(3)\n}",
        "mismatched types",
    );
}

#[test]
fn turbofish() {
    ok("fn make<T>() -> Vec<T> {\n return Vec::new()\n}\nfn main() {\n v := make::<u8>()\n}");
}

#[test]
fn dyn_trait_coercion() {
    ok(&format!("{TRAITS}\nfn show(x: &dyn Describe) -> String {{\n return x.describe()\n}}\nfn main() {{\n d := Dog {{ name: String::from(\"a\") }}\n s := show(&d)\n}}"));
    has(
        &format!("{TRAITS}\nfn show(x: &dyn Describe) {{}}\nfn main() {{\n show(&3)\n}}"),
        "mismatched types",
    );
}

#[test]
fn inherent_impl_on_foreign_type() {
    err1(
        "impl i32 {\n fn twice(&self) -> i32 {\n return 2\n }\n}\nfn main() {}",
        "cannot define inherent `impl`",
    );
}

#[test]
fn duplicate_methods() {
    has(
        "struct S\nimpl S {\n fn a(&self) {}\n}\nimpl S {\n fn a(&self) {}\n}\nfn main() {}",
        "duplicate definitions",
    );
}

#[test]
fn trait_as_type_is_an_error() {
    has(
        "trait T {}\nfn f(x: &T) {}\nfn main() {}",
        "trait `T` used as a type",
    );
}

// ----- Result and ? -------------------------------------------------------------------------

#[test]
fn question_mark_propagates() {
    ok("fn parse(s: &str) -> Result<i32, String> {\n return Ok(1)\n}\nfn twice(s: &str) -> Result<i32, String> {\n n := parse(s)?\n return Ok(n * 2)\n}\nfn main() {}");
}

#[test]
fn question_mark_in_non_result_fn() {
    err1(
        "fn parse() -> Result<i32, String> {\n return Ok(1)\n}\nfn main() {\n n := parse()?\n}",
        "can only be used in a function that returns `Result`",
    );
}

#[test]
fn question_mark_error_conversion() {
    let base = "struct E1\nstruct E2\nfn a() -> Result<i32, E1> {\n return Err(E1)\n}\n";
    err1(
        &format!(
            "{base}fn b() -> Result<i32, E2> {{\n x := a()?\n return Ok(x)\n}}\nfn main() {{}}"
        ),
        "couldn't convert the error to `E2`",
    );
    ok(&format!(
        "{base}impl From<E1> for E2 {{\n fn from(e: E1) -> E2 {{\n return E2\n }}\n}}\nfn b() -> Result<i32, E2> {{\n x := a()?\n return Ok(x)\n}}\nfn main() {{}}"
    ));
}

#[test]
fn question_mark_on_option() {
    ok("fn f(v: Option<i32>) -> Option<i32> {\n x := v?\n return Some(x + 1)\n}\nfn main() {}");
    has(
        "fn f(x: i32) -> Result<i32, String> {\n y := x?\n return Ok(y)\n}\nfn main() {}",
        "can only be applied to `Result` or `Option`",
    );
}

#[test]
fn main_may_return_result() {
    ok("fn main() -> Result<(), String> {\n return Ok(())\n}");
    err1(
        "fn main() -> i32 {\n return 0\n}",
        "`main` has invalid return type `i32`",
    );
}

// ----- control flow -----------------------------------------------------------------------

#[test]
fn conditions_must_be_bool() {
    err1(
        "fn main() {\n if 1 {\n }\n}",
        "expected `bool`, found `{integer}`",
    );
    err1(
        "fn main() {\n while \"x\" {\n }\n}",
        "expected `bool`, found `&str`",
    );
}

#[test]
fn for_loops() {
    ok("fn main() {\n for i in 0..10 {\n let x: u8 = i\n }\n v := [1, 2, 3]\n for x in &v {\n y := *x + 1\n }\n}");
    err1(
        "fn main() {\n for i in true {\n }\n}",
        "`bool` is not iterable",
    );
}

#[test]
fn closures() {
    ok("fn apply(f: fn(i32) -> i32, x: i32) -> i32 {\n return f(x)\n}\nfn main() {\n double := |x| x * 2\n y := apply(double, 3)\n}");
    ok("fn main() {\n f := |x: i32| {\n return x + 1\n }\n let y: i32 = f(1)\n}");
    has(
        "fn main() {\n f := |x: i32| x + 1\n let y: bool = f(1)\n}",
        "mismatched types",
    );
}

#[test]
fn calling_a_non_function() {
    err1("fn main() {\n x := 1\n x()\n}", "expected function");
}

// ----- concurrency ------------------------------------------------------------------------

#[test]
fn channels_send_and_receive() {
    ok("fn main() {\n (tx, rx) := chan::<i32>(10)\n tx <- 5\n v := <-rx\n let w: i32 = v\n}");
    err1(
        "fn main() {\n (tx, rx) := chan::<i32>(10)\n tx <- true\n}",
        "expected `i32`, found `bool`",
    );
}

#[test]
fn channel_direction_is_enforced() {
    has(
        "fn main() {\n (tx, rx) := chan::<i32>(1)\n rx <- 1\n}",
        "cannot send on a value of type `<-chan i32`",
    );
    has(
        "fn main() {\n (tx, rx) := chan::<i32>(1)\n v := <-tx\n}",
        "cannot receive from",
    );
}

#[test]
fn channel_types_in_signatures() {
    ok("fn worker(jobs: <-chan u32, out: chan<- String) {\n for j in jobs {\n out <- \"{j}\"\n }\n}\nfn main() {\n (jtx, jrx) := chan::<u32>(4)\n (otx, orx) := chan::<String>(4)\n go worker(jrx, otx)\n jtx <- 1\n}");
}

#[test]
fn select_arms() {
    ok("fn main() {\n (tx, rx) := chan::<i32>(1)\n (dtx, done) := chan::<bool>(1)\n select {\n v := <-rx => println(\"{v}\"),\n tx <- 3 => {},\n <-done => {},\n }\n}");
    has(
        "fn main() {\n (tx, rx) := chan::<i32>(1)\n select {\n tx <- \"x\" => {},\n }\n}",
        "expected `i32`, found `&str`",
    );
}

#[test]
fn chan_methods() {
    ok("fn main() {\n (tx, rx) := chan::<i32>(1)\n r := tx.try_send(1)\n o := rx.try_recv()\n p := rx.recv()\n let q: Option<i32> = p\n tx.close()\n let n: usize = rx.len()\n}");
}

#[test]
fn go_blocks_and_defer() {
    ok("fn main() {\n (tx, rx) := chan::<i32>(1)\n go {\n tx <- 1\n }\n defer println(\"bye\")\n}");
}

// ----- whole examples ---------------------------------------------------------------------------

#[test]
fn repository_examples_type_check() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "kn") {
            let src = std::fs::read_to_string(&path).unwrap();
            let e = errs(&src);
            assert!(e.is_empty(), "{}: {e:#?}", path.display());
        }
    }
}

/// Every ```kiln block in the website must type-check (use ```text for
/// intentionally broken code).
#[test]
fn website_examples_type_check() {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|e| e == "md") {
                out.push(p);
            }
        }
    }
    let root = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../website/src"));
    let mut files = Vec::new();
    walk(root, &mut files);
    let mut checked = 0;
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        for (i, block) in text.split("```kiln\n").skip(1).enumerate() {
            let code = block.split("```").next().unwrap();
            let e = errs(code);
            assert!(
                e.is_empty(),
                "{} block {}: {e:#?}\n{code}",
                f.display(),
                i + 1
            );
            checked += 1;
        }
    }
    assert!(checked > 20, "only {checked} website examples found");
}

#[test]
fn array_lengths_are_part_of_the_type() {
    ok("fn main() {\n let a: [i32; 3] = [1, 2, 3]\n z := [0u8; 16]\n n := a.len() + z.len()\n}");
    has(
        "fn main() {\n let a: [i32; 3] = [1, 2]\n}",
        "expected `[i32; 3]`, found `[{integer}; 2]`",
    );
    has(
        "fn main() {\n s := [String::from(\"a\"); 2]\n}",
        "`[x; 2]` needs a `Copy` element type, found `String`",
    );
    has(
        "const N: usize = 3\nfn main() {\n let a: [i32; N] = [1, 2, 3]\n}",
        "array lengths must be integer literals for now",
    );
}

#[test]
fn exhaustiveness_looks_inside_patterns() {
    let opt = "enum E {\n A(bool),\n B,\n}\n";
    ok(&format!("{opt}fn f(e: E) -> i32 {{\n return match e {{\n E::A(true) => 1,\n E::A(false) => 2,\n E::B => 3,\n }}\n}}"));
    has(
        &format!("{opt}fn f(e: E) -> i32 {{\n return match e {{\n E::A(true) => 1,\n E::B => 3,\n }}\n}}"),
        "non-exhaustive match: `A` not covered",
    );
    // Nested enums, tuples, and the unit pattern.
    ok("fn f(r: Result<(), Option<i32>>) -> i32 {\n return match r {\n Ok(()) => 0,\n Err(Some(_)) => 1,\n Err(None) => 2,\n }\n}");
    ok("fn f(p: (bool, bool)) -> i32 {\n return match p {\n (true, _) => 1,\n (false, true) => 2,\n (false, false) => 3,\n }\n}");
    has(
        "fn f(p: (bool, bool)) -> i32 {\n return match p {\n (true, _) => 1,\n (false, true) => 2,\n }\n}",
        "non-exhaustive match: not all values of `(bool, bool)` are covered",
    );
    // Integers need a wildcard.
    has(
        "fn f(n: i32) -> i32 {\n return match n {\n 0 => 1,\n 1 => 2,\n }\n}",
        "non-exhaustive match: not all values of `i32` are covered",
    );
}

#[test]
fn enum_associated_functions() {
    ok("enum E {\n\tA,\n\tB,\n}\nimpl E {\n\tfn first() -> E {\n\t\treturn E::A\n\t}\n}\nfn f() {\n\tx := E::first()\n}");
    assert_eq!(
        errs("enum E {\n\tA,\n}\nfn f() {\n\tx := E::B\n}"),
        vec!["no variant or associated function named `B` in enum `E`"]
    );
}

#[test]
fn unsafe_operations_need_unsafe() {
    let src = "unsafe fn danger() {}\nfn main() {\n x mut := 1\n p := &mut x as *mut i32\n *p = 2\n danger()\n unsafe {\n  *p = 3\n  danger()\n }\n}";
    assert_eq!(
        errs(src),
        vec![
            "dereferencing a raw pointer is unsafe and requires an `unsafe` block",
            "calling `unsafe fn danger` is unsafe and requires an `unsafe` block",
        ]
    );
    // Inside an `unsafe fn`, no block is needed.
    assert!(errs("unsafe fn f(p: *mut i32) {\n *p = 1\n}\nfn main() {}").is_empty());
}

#[test]
fn statics() {
    assert!(errs("static N: i32 = 4\nstatic S: &str = \"x\"\nstatic mut M: i32 = 0\nfn main() {\n x := N + 1\n unsafe {\n  M = x\n }\n}").is_empty());
    assert_eq!(
        errs("static mut M: i32 = 0\nfn main() {\n M = 1\n}"),
        vec!["using a `static mut` is unsafe and requires an `unsafe` block"]
    );
    assert_eq!(
        errs("static N: i32 = 0\nfn main() {\n N = 1\n}"),
        vec!["cannot assign to the immutable static `N`"]
    );
    assert_eq!(
        errs("static P: *mut i32 = 0 as *mut i32\nfn main() {}"),
        vec!["static `P` has type `*mut i32`, which cannot be shared between tasks safely"]
    );
    assert_eq!(
        errs("static N: i32 = \"no\"\nfn main() {}"),
        vec!["mismatched types: expected `i32`, found `&str`"]
    );
}
