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
    ok("fun main() {\n println(\"hi\")\n}");
}

#[test]
fn literal_inference_and_annotations() {
    ok("fun main() {\n let a u8 = 3\n b := 2.5\n let c f32 = b\n d := a + 1\n let e u8 = d\n}");
}

#[test]
fn mismatched_let_annotation() {
    err1(
        "fun main() {\n let a bool = 3\n}",
        "expected `bool`, found `{integer}`",
    );
}

#[test]
fn int_and_float_do_not_mix() {
    err1(
        "fun main() {\n x := 1 + 2.0\n}",
        "cannot apply `+` to `{integer}` and `{float}`",
    );
}

#[test]
fn casts() {
    ok("fun main() {\n x := 3\n y := x as f64\n c := 65u8 as char\n n := 'a' as u32\n println(\"{y}{c}{n}\")\n}");
    err1(
        "fun main() {\n b := true as f64\n}",
        "invalid cast from `bool` to `f64`",
    );
}

#[test]
fn string_interpolation_is_string() {
    ok("fun f() string {\n x := 1\n return \"{x}\"\n}\nfun main() {}");
}

#[test]
fn string_concat_and_deref_coercion() {
    ok("fun takes(s &str) usize {\n return s.len()\n}\nfun main() {\n s := string::from(\"a\") + \"b\"\n n := takes(&s)\n println(\"{n}\")\n}");
    has(
        "fun takes(s &str) {}\nfun main() {\n s := string::from(\"a\")\n takes(s)\n}",
        "expected `&str`, found `string`",
    );
}

#[test]
fn unresolved_type_needs_annotation() {
    err1(
        "fun main() {\n v := Vec::new()\n}",
        "type annotations needed for `v`",
    );
    ok("fun main() {\n v mut := Vec::new()\n v.push(1)\n}");
    ok("fun main() {\n let v Vec<u8> = Vec::new()\n}");
}

// ----- explicit return (SPEC §3.1) ---------------------------------------------------

#[test]
fn explicit_return_required() {
    err1("fun f() i32 {\n 1\n}\nfun main() {}", "missing `return`");
}

#[test]
fn missing_return_on_some_path() {
    err1(
        "fun f(b bool) i32 {\n if b {\n return 1\n }\n}\nfun main() {}",
        "must return a value of type `i32` on every path",
    );
}

#[test]
fn all_paths_returning_is_fine() {
    ok("fun f(b bool) i32 {\n if b {\n return 1\n } else {\n return 2\n }\n}\nfun main() {}");
    ok("fun f(x i32) i32 {\n match x {\n 0 => return 1,\n _ => return 2,\n }\n}\nfun main() {}");
    ok("fun f() i32 {\n loop {\n return 3\n }\n}\nfun main() {}");
    ok("fun f() i32 {\n panic(\"no\")\n}\nfun main() {}");
}

#[test]
fn unit_function_returning_value() {
    err1(
        "fun f() {\n return 5\n}\nfun main() {}",
        "this function returns `void`",
    );
}

#[test]
fn wrong_return_type() {
    err1(
        "fun f() bool {\n return 5\n}\nfun main() {}",
        "expected `bool`, found `{integer}`",
    );
    err1(
        "fun f() i32 {\n return\n}\nfun main() {}",
        "`return` without a value",
    );
}

#[test]
fn if_and_match_as_values() {
    ok("fun f(b bool) i32 {\n x := if b { 1 } else { 2 }\n y := match x {\n 1 => 10,\n _ => 20,\n }\n return y\n}\nfun main() {}");
    has(
        "fun f(b bool) {\n x := if b { 1 } else { \"no\" }\n}\nfun main() {}",
        "incompatible types",
    );
    has(
        "fun f(b bool) {\n let x i32 = if b { 1 }\n}\nfun main() {}",
        "`if` without `else`",
    );
}

#[test]
fn loop_break_value() {
    ok("fun main() {\n i mut := 0\n x := loop {\n i += 1\n if i > 3 {\n break i * 2\n }\n }\n let y i64 = x\n}");
    has(
        "fun main() {\n while true {\n break 3\n }\n}",
        "only allowed inside `loop`",
    );
}

// ----- := rules ---------------------------------------------------------------------

#[test]
fn short_decl_tuple_destructuring() {
    ok("fun pair() (i32, bool) {\n return (1, true)\n}\nfun main() {\n (a, b) := pair()\n let x bool = b\n let y i32 = a\n}");
    err1("fun main() {\n (a, b) := 5\n}", "cannot destructure");
}

#[test]
fn short_decl_of_unit_is_an_error() {
    err1("fun f() {}\nfun main() {\n x := f()\n}", "type `void`");
}

#[test]
fn immutable_assignment() {
    err1(
        "fun main() {\n x := 1\n x = 2\n}",
        "cannot assign twice to immutable variable `x`",
    );
    ok("fun main() {\n x mut := 1\n x = 2\n x += 1\n}");
    ok("fun main() {\n let mut x = 1\n x = 2\n}");
}

#[test]
fn mutable_borrow_requires_mut_binding() {
    err1(
        "fun main() {\n v := Vec::<i32>::new()\n r := &mut v\n}",
        "cannot borrow `v`",
    );
}

#[test]
fn mut_self_method_requires_mut_binding() {
    has(
        "fun main() {\n let v Vec<i32> = Vec::new()\n v.push(1)\n}",
        "cannot borrow `v`",
    );
}

#[test]
fn assignment_through_shared_ref() {
    has(
        "struct P {\n x i32,\n}\nfun f(p &P) {\n p.x = 3\n}\nfun main() {}",
        "behind a `&` reference",
    );
    ok("struct P {\n x i32,\n}\nfun f(p &mut P) {\n p.x = 3\n}\nfun main() {}");
}

// ----- structs and enums --------------------------------------------------------------

const SHAPES: &str = "
struct Point {
\tx f64,
\ty f64,
}

enum Shape {
\tCircle(f64),
\tRect { w f64, h f64 },
\tEmpty,
}

impl Point {
\tfun new(x f64, y f64) Point {
\t\treturn Point { x: x, y: y }
\t}
\tfun len(&self) f64 {
\t\treturn self.x * self.x + self.y * self.y
\t}
\tfun scale(&mut self, k f64) {
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
    ok(&with_shapes("fun main() {\n p mut := Point::new(1.0, 2.0)\n p.scale(2.0)\n let l f64 = p.len()\n x := p.x\n}"));
}

#[test]
fn struct_literal_errors() {
    has(
        &with_shapes("fun main() {\n p := Point { x: 1.0 }\n}"),
        "missing field `y`",
    );
    has(
        &with_shapes("fun main() {\n p := Point { x: 1.0, y: 2.0, z: 3.0 }\n}"),
        "no field named `z`",
    );
    has(
        &with_shapes("fun main() {\n p := Point { x: 1.0, y: true }\n}"),
        "expected `f64`, found `bool`",
    );
}

#[test]
fn unknown_field_and_method() {
    has(
        &with_shapes("fun main() {\n p := Point::new(1.0, 2.0)\n z := p.z\n}"),
        "no field `z` on type `Point`",
    );
    has(
        &with_shapes("fun main() {\n p := Point::new(1.0, 2.0)\n p.fly()\n}"),
        "no method named `fly`",
    );
}

#[test]
fn wrong_argument_count_and_type() {
    has(
        &with_shapes("fun main() {\n p := Point::new(1.0)\n}"),
        "takes 2 arguments but 1 was supplied",
    );
    has(
        &with_shapes("fun main() {\n p := Point::new(1.0, \"x\")\n}"),
        "expected `f64`, found `&str`",
    );
}

#[test]
fn enum_match_with_ref_ergonomics() {
    ok(&with_shapes(
        "fun area(s &Shape) f64 {\n return match s {\n Shape::Circle(r) => 3.0 * r * r,\n Shape::Rect { w, h } => w * h,\n Shape::Empty => 0.0,\n }\n}\nfun main() {}",
    ));
}

#[test]
fn non_exhaustive_match() {
    err1(
        &with_shapes("fun f(s Shape) i32 {\n match s {\n Shape::Circle(_) => return 1,\n Shape::Empty => return 2,\n }\n}\nfun main() {}"),
        "non-exhaustive match: `Rect` not covered",
    );
    err1(
        "fun f(x i32) {\n match x {\n 1 => {},\n }\n}\nfun main() {}",
        "not all values of `i32`",
    );
    ok("fun f(b bool) i32 {\n match b {\n true => return 1,\n false => return 0,\n }\n}\nfun main() {}");
}

#[test]
fn guarded_arms_do_not_count() {
    has("fun f(x option<i32>) {\n match x {\n Some(v) if v > 0 => {},\n None => {},\n }\n}\nfun main() {}", "`Some` not covered");
}

#[test]
fn pattern_arity_and_fields() {
    has(&with_shapes("fun f(s Shape) {\n match s {\n Shape::Circle(a, b) => {},\n _ => {},\n }\n}\nfun main() {}"), "this pattern has 2 fields");
    has(&with_shapes("fun f(s Shape) {\n match s {\n Shape::Rect { w } => {},\n _ => {},\n }\n}\nfun main() {}"), "pattern does not mention field `h`");
    ok(&with_shapes("fun f(s Shape) {\n match s {\n Shape::Rect { w, .. } => {},\n _ => {},\n }\n}\nfun main() {}"));
}

#[test]
fn generic_enum_and_option() {
    ok("fun first(v &Vec<i32>) option<i32> {\n if v.is_empty() {\n return None\n }\n return Some(v[0])\n}\nfun main() {\n x := first(&Vec::new()).unwrap_or(0)\n let y i32 = x\n}");
    has(
        "fun f() option<i32> {\n return Some(true)\n}\nfun main() {}",
        "mismatched types: expected `i32`, found `bool`",
    );
}

#[test]
fn generic_struct() {
    ok("struct Pair<T> {\n a T,\n b T,\n}\nimpl<T> Pair<T> {\n fun first(&self) &T {\n return &self.a\n }\n}\nfun main() {\n p := Pair { a: 1u8, b: 2 }\n let x &u8 = p.first()\n}");
    has(
        "struct Pair<T> {\n a T,\n b T,\n}\nfun main() {\n p := Pair { a: 1, b: true }\n}",
        "expected `{integer}`, found `bool`",
    );
    has(
        "struct W<T> {\n v T,\n}\nfun f(w W) {}\nfun main() {}",
        "takes 1 generic argument but 0 were supplied",
    );
}

#[test]
fn tuple_structs_and_index() {
    ok("struct Meters(f64)\nfun main() {\n m := Meters(3.0)\n let x f64 = m.0\n t := (1, true)\n let b bool = t.1\n}");
    has(
        "fun main() {\n t := (1, true)\n x := t.2\n}",
        "no field `2`",
    );
}

// ----- traits and generics ---------------------------------------------------------------

const TRAITS: &str = "
trait Describe {
\tfun describe(&self) string
\tfun shout(&self) string {
\t\treturn self.describe().to_upper()
\t}
}

struct Dog {
\tname string,
}

impl Describe for Dog {
\tfun describe(&self) string {
\t\treturn \"dog {self.name}\"
\t}
}
";

#[test]
fn trait_impl_and_default_method() {
    ok(&format!("{TRAITS}\nfun main() {{\n d := Dog {{ name: string::from(\"rex\") }}\n s := d.shout()\n t := d.describe()\n}}"));
}

#[test]
fn missing_trait_method() {
    err1("trait T {\n fun a(&self)\n fun b(&self)\n}\nstruct S\nimpl T for S {\n fun a(&self) {}\n}\nfun main() {}", "missing: `b`");
}

#[test]
fn extra_trait_method() {
    err1("trait T {\n fun a(&self)\n}\nstruct S\nimpl T for S {\n fun a(&self) {}\n fun z(&self) {}\n}\nfun main() {}", "`z` is not a member of trait `T`");
}

#[test]
fn incompatible_trait_signature() {
    err1("trait T {\n fun a(&self) i32\n}\nstruct S\nimpl T for S {\n fun a(&self) bool {\n return true\n }\n}\nfun main() {}", "incompatible signature");
    err1("trait T {\n fun a(&self)\n}\nstruct S\nimpl T for S {\n fun a(&mut self) {}\n}\nfun main() {}", "incompatible signature");
}

#[test]
fn generic_bound_methods() {
    ok(&format!("{TRAITS}\nfun loud<T: Describe>(x &T) string {{\n return x.shout()\n}}\nfun main() {{\n d := Dog {{ name: string::from(\"a\") }}\n s := loud(&d)\n}}"));
    has(
        "fun f<T>(x T) {\n x.describe()\n}\nfun main() {}",
        "no method named `describe`",
    );
}

#[test]
fn trait_bounds_checked_at_call_sites() {
    let src =
        "fun biggest<T: Ord>(a T, b T) T {\n if a > b {\n return a\n }\n return b\n}\nstruct P\n";
    ok(&format!("{src}fun main() {{\n x := biggest(1, 2)\n}}"));
    err1(
        &format!("{src}fun main() {{\n x := biggest(P, P)\n}}"),
        "the trait `Ord` is not implemented for `P`",
    );
}

#[test]
fn derive_satisfies_bounds() {
    ok("@derive(Eq, Ord)\nstruct P\nfun big<T: Ord>(a T) {}\nfun main() {\n big(P)\n}");
}

#[test]
fn comparisons_need_eq() {
    err1(
        "struct P\nfun main() {\n b := P == P\n}",
        "`Eq` is not implemented for `P`",
    );
    ok("@derive(Eq)\nstruct P\nfun main() {\n b := P == P\n}");
    ok("fun main() {\n b := 1.0 < 2.0\n c := \"a\" == \"b\"\n}");
}

#[test]
fn generic_inference_across_calls() {
    ok("fun id<T>(x T) T {\n return x\n}\nfun main() {\n let a u64 = id(3)\n b := id(true)\n let c bool = b\n}");
    has(
        "fun id<T>(x T) T {\n return x\n}\nfun main() {\n let a bool = id(3)\n}",
        "mismatched types",
    );
}

#[test]
fn turbofish() {
    ok("fun make<T>() Vec<T> {\n return Vec::new()\n}\nfun main() {\n v := make::<u8>()\n}");
}

#[test]
fn dyn_trait_coercion() {
    ok(&format!("{TRAITS}\nfun show(x &dyn Describe) string {{\n return x.describe()\n}}\nfun main() {{\n d := Dog {{ name: string::from(\"a\") }}\n s := show(&d)\n}}"));
    has(
        &format!("{TRAITS}\nfun show(x &dyn Describe) {{}}\nfun main() {{\n show(&3)\n}}"),
        "mismatched types",
    );
}

#[test]
fn inherent_impl_on_foreign_type() {
    err1(
        "impl i32 {\n fun twice(&self) i32 {\n return 2\n }\n}\nfun main() {}",
        "cannot define inherent `impl`",
    );
}

#[test]
fn duplicate_methods() {
    has(
        "struct S\nimpl S {\n fun a(&self) {}\n}\nimpl S {\n fun a(&self) {}\n}\nfun main() {}",
        "duplicate definitions",
    );
}

#[test]
fn trait_as_type_is_an_error() {
    has(
        "trait T {}\nfun f(x &T) {}\nfun main() {}",
        "trait `T` used as a type",
    );
}

// ----- Result and ? -------------------------------------------------------------------------

#[test]
fn question_mark_propagates() {
    ok("fun parse(s &str) result<i32, string> {\n return Ok(1)\n}\nfun twice(s &str) result<i32, string> {\n n := parse(s)?\n return Ok(n * 2)\n}\nfun main() {}");
}

#[test]
fn question_mark_in_non_result_fn() {
    err1(
        "fun parse() result<i32, string> {\n return Ok(1)\n}\nfun main() {\n n := parse()?\n}",
        "can only be used in a function that returns `result`",
    );
}

#[test]
fn question_mark_error_conversion() {
    let base = "struct E1\nstruct E2\nfun a() result<i32, E1> {\n return Err(E1)\n}\n";
    err1(
        &format!(
            "{base}fun b() result<i32, E2> {{\n x := a()?\n return Ok(x)\n}}\nfun main() {{}}"
        ),
        "couldn't convert the error to `E2`",
    );
    ok(&format!(
        "{base}impl From<E1> for E2 {{\n fun from(e E1) E2 {{\n return E2\n }}\n}}\nfun b() result<i32, E2> {{\n x := a()?\n return Ok(x)\n}}\nfun main() {{}}"
    ));
}

#[test]
fn question_mark_on_option() {
    ok("fun f(v option<i32>) option<i32> {\n x := v?\n return Some(x + 1)\n}\nfun main() {}");
    has(
        "fun f(x i32) result<i32, string> {\n y := x?\n return Ok(y)\n}\nfun main() {}",
        "can only be applied to `result` or `option`",
    );
}

#[test]
fn main_may_return_result() {
    ok("fun main() result<void, string> {\n return Ok()\n}");
    err1(
        "fun main() i32 {\n return 0\n}",
        "`main` has invalid return type `i32`",
    );
}

// ----- control flow -----------------------------------------------------------------------

#[test]
fn conditions_must_be_bool() {
    err1(
        "fun main() {\n if 1 {\n }\n}",
        "expected `bool`, found `{integer}`",
    );
    err1(
        "fun main() {\n while \"x\" {\n }\n}",
        "expected `bool`, found `&str`",
    );
}

#[test]
fn for_loops() {
    ok("fun main() {\n for i in 0..10 {\n let x u8 = i\n }\n v := [1, 2, 3]\n for x in &v {\n y := *x + 1\n }\n}");
    err1(
        "fun main() {\n for i in true {\n }\n}",
        "`bool` is not iterable",
    );
}

#[test]
fn closures() {
    ok("fun apply(f fun(i32) i32, x i32) i32 {\n return f(x)\n}\nfun main() {\n double := |x| x * 2\n y := apply(double, 3)\n}");
    ok("fun main() {\n f := |x i32| {\n return x + 1\n }\n let y i32 = f(1)\n}");
    has(
        "fun main() {\n f := |x i32| x + 1\n let y bool = f(1)\n}",
        "mismatched types",
    );
}

#[test]
fn calling_a_non_function() {
    err1("fun main() {\n x := 1\n x()\n}", "expected function");
}

// ----- concurrency ------------------------------------------------------------------------

#[test]
fn channels_send_and_receive() {
    ok("fun main() {\n (tx, rx) := chan::<i32>(10)\n tx <- 5\n v := <-rx\n let w i32 = v\n}");
    err1(
        "fun main() {\n (tx, rx) := chan::<i32>(10)\n tx <- true\n}",
        "expected `i32`, found `bool`",
    );
}

#[test]
fn channel_direction_is_enforced() {
    has(
        "fun main() {\n (tx, rx) := chan::<i32>(1)\n rx <- 1\n}",
        "cannot send on a value of type `<-chan i32`",
    );
    has(
        "fun main() {\n (tx, rx) := chan::<i32>(1)\n v := <-tx\n}",
        "cannot receive from",
    );
}

#[test]
fn channel_types_in_signatures() {
    ok("fun worker(jobs <-chan u32, out chan<- string) {\n for j in jobs {\n out <- \"{j}\"\n }\n}\nfun main() {\n (jtx, jrx) := chan::<u32>(4)\n (otx, orx) := chan::<string>(4)\n go worker(jrx, otx)\n jtx <- 1\n}");
}

#[test]
fn select_arms() {
    ok("fun main() {\n (tx, rx) := chan::<i32>(1)\n (dtx, done) := chan::<bool>(1)\n select {\n v := <-rx => println(\"{v}\"),\n tx <- 3 => {},\n <-done => {},\n }\n}");
    has(
        "fun main() {\n (tx, rx) := chan::<i32>(1)\n select {\n tx <- \"x\" => {},\n }\n}",
        "expected `i32`, found `&str`",
    );
}

#[test]
fn chan_methods() {
    ok("fun main() {\n (tx, rx) := chan::<i32>(1)\n r := tx.try_send(1)\n o := rx.try_recv()\n p := rx.recv()\n let q option<i32> = p\n tx.close()\n let n usize = rx.len()\n}");
}

#[test]
fn go_blocks_and_defer() {
    ok("fun main() {\n (tx, rx) := chan::<i32>(1)\n go {\n tx <- 1\n }\n defer println(\"bye\")\n}");
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
    ok("fun main() {\n let a [i32; 3] = [1, 2, 3]\n z := [0u8; 16]\n n := a.len() + z.len()\n}");
    has(
        "fun main() {\n let a [i32; 3] = [1, 2]\n}",
        "expected `[i32; 3]`, found `[{integer}; 2]`",
    );
    has(
        "fun main() {\n s := [string::from(\"a\"); 2]\n}",
        "`[x; 2]` needs a `Copy` element type, found `string`",
    );
    has(
        "const N usize = 3\nfun main() {\n let a [i32; N] = [1, 2, 3]\n}",
        "array lengths must be integer literals for now",
    );
}

#[test]
fn exhaustiveness_looks_inside_patterns() {
    let opt = "enum E {\n A(bool),\n B,\n}\n";
    ok(&format!("{opt}fun f(e E) i32 {{\n return match e {{\n E::A(true) => 1,\n E::A(false) => 2,\n E::B => 3,\n }}\n}}"));
    has(
        &format!(
            "{opt}fun f(e E) i32 {{\n return match e {{\n E::A(true) => 1,\n E::B => 3,\n }}\n}}"
        ),
        "non-exhaustive match: `A` not covered",
    );
    // Nested enums, tuples, and the unit pattern.
    ok("fun f(r result<void, option<i32>>) i32 {\n return match r {\n Ok() => 0,\n Err(Some(_)) => 1,\n Err(None) => 2,\n }\n}");
    ok("fun f(p (bool, bool)) i32 {\n return match p {\n (true, _) => 1,\n (false, true) => 2,\n (false, false) => 3,\n }\n}");
    has(
        "fun f(p (bool, bool)) i32 {\n return match p {\n (true, _) => 1,\n (false, true) => 2,\n }\n}",
        "non-exhaustive match: not all values of `(bool, bool)` are covered",
    );
    // Integers need a wildcard.
    has(
        "fun f(n i32) i32 {\n return match n {\n 0 => 1,\n 1 => 2,\n }\n}",
        "non-exhaustive match: not all values of `i32` are covered",
    );
}

#[test]
fn enum_associated_functions() {
    ok("enum E {\n\tA,\n\tB,\n}\nimpl E {\n\tfun first() E {\n\t\treturn E::A\n\t}\n}\nfun f() {\n\tx := E::first()\n}");
    assert_eq!(
        errs("enum E {\n\tA,\n}\nfun f() {\n\tx := E::B\n}"),
        vec!["no variant or associated function named `B` in enum `E`"]
    );
}

#[test]
fn unsafe_operations_need_unsafe() {
    let src = "unsafe fun danger() {}\nfun main() {\n x mut := 1\n p := &mut x as *mut i32\n *p = 2\n danger()\n unsafe {\n  *p = 3\n  danger()\n }\n}";
    assert_eq!(
        errs(src),
        vec![
            "dereferencing a raw pointer is unsafe and requires an `unsafe` block",
            "calling `unsafe fun danger` is unsafe and requires an `unsafe` block",
        ]
    );
    // Inside an `unsafe fn`, no block is needed.
    assert!(errs("unsafe fun f(p *mut i32) {\n *p = 1\n}\nfun main() {}").is_empty());
}

#[test]
fn statics() {
    assert!(errs("static N i32 = 4\nstatic S &str = \"x\"\nstatic mut M i32 = 0\nfun main() {\n x := N + 1\n unsafe {\n  M = x\n }\n}").is_empty());
    assert_eq!(
        errs("static mut M i32 = 0\nfun main() {\n M = 1\n}"),
        vec!["using a `static mut` is unsafe and requires an `unsafe` block"]
    );
    assert_eq!(
        errs("static N i32 = 0\nfun main() {\n N = 1\n}"),
        vec!["cannot assign to the immutable static `N`"]
    );
    assert_eq!(
        errs("static P *mut i32 = 0 as *mut i32\nfun main() {}"),
        vec!["static `P` has type `*mut i32`, which cannot be shared between tasks safely"]
    );
    assert_eq!(
        errs("static N i32 = \"no\"\nfun main() {}"),
        vec!["mismatched types: expected `i32`, found `&str`"]
    );
}

#[test]
fn closure_mutability_inference_and_calls_through_references() {
    // An un-annotated closure that mutates a capture is a `fn mut`, so it
    // can be passed as `&mut fn mut(..)`.
    ok("fun twice(f &mut fun mut(i32) i32, x i32) i32 {\n return f(f(x))\n}\nfun main() {\n n mut := 0\n g mut := |x i32| {\n  n += 1\n  return x\n }\n y := twice(&mut g, 1)\n}");
    // One that only reads is a `fn`, callable through `&`.
    ok("fun call(f &fun(i32) i32) i32 {\n return f(1)\n}\nfun main() {\n k := 2\n g := |x i32| x * k\n y := call(&g)\n}");
    assert_eq!(
        errs("fun call(f &fun mut(i32) i32) i32 {\n return f(1)\n}\nfun main() {}"),
        vec!["cannot call a `fun mut` through a shared reference"]
    );
}

#[test]
fn associated_and_computed_constants() {
    ok("struct S {}\nimpl S {\n pub const N i32 = 2 * 3\n}\nconst M i32 = S::N + 1\nfun main() {\n x := S::N + M + i32::MAX - u8::MAX as i32\n}");
    assert_eq!(
        errs("const C i32 = \"s\"\nfun main() {}"),
        vec!["mismatched types: expected `i32`, found `&str`"]
    );
}

#[test]
fn format_specs() {
    ok("@derive(Debug)\nstruct P {\n x i32,\n}\nfun main() {\n n := 5\n s := \"{n:>4} {n:#x} {1.5:.2} {1.5:e} {P { x: 1 }:?} {\"a\":^5}\"\n}");
    assert_eq!(
        errs("struct Q {}\nfun main() {\n q := Q {}\n s := \"{q:?} {\"a\":x} {true:e}\"\n}"),
        vec![
            "`Q` cannot be formatted with `:?` because it does not implement `Debug`",
            "hexadecimal, octal, and binary formats need an integer, found `str`",
            "scientific notation needs a number, found `bool`",
        ]
    );
}

#[test]
fn turbofish_on_variants() {
    ok("fun main() {\n x := None::<i32>\n y := x.is_none()\n}");
}

#[test]
fn integer_literals_must_fit_their_type() {
    err1(
        "fun main() {\n x := 3000000000\n}",
        "literal out of range for `i32`",
    );
    err1(
        "fun main() {\n let x u8 = 256\n}",
        "literal out of range for `u8`",
    );
    err1(
        "fun main() {\n x := 60000000000 as u64\n}",
        "literal out of range for `i32`",
    );
    err1(
        "fun main() {\n let x i8 = -129\n}",
        "literal out of range for `i8`",
    );
    ok("fun main() {\n let a i8 = -128\n let b u8 = 255\n c := 60000000000u64\n let d i64 = -9223372036854775808\n e := 0xFFFFFFFFu32\n let f u64 = 18446744073709551615\n}");
}
