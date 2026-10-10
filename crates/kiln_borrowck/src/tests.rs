//! Borrow checker tests. Each test is a small Kiln program.

use crate::check_source;

fn errs(src: &str) -> Vec<String> {
    check_source(src).into_iter().map(|e| e.message).collect()
}

#[track_caller]
fn ok(src: &str) {
    let e = check_source(src);
    assert!(
        e.is_empty(),
        "unexpected errors:\n{}",
        e.iter().map(|d| d.render("t.kn", src)).collect::<String>()
    );
}

/// Exactly one error, containing `needle`.
#[track_caller]
fn err1(src: &str, needle: &str) {
    let e = errs(src);
    assert!(
        e.len() == 1 && e[0].contains(needle),
        "expected one error containing {needle:?}, got {e:#?}"
    );
}

#[track_caller]
fn has(src: &str, needle: &str) {
    let e = errs(src);
    assert!(
        e.iter().any(|m| m.contains(needle)),
        "expected an error containing {needle:?}, got {e:#?}"
    );
}

fn main_with(body: &str) -> String {
    format!("fn take(s: String) {{}}\nfn read(s: &String) {{}}\nfn main() {{\n{body}\n}}")
}

// ----- moves -------------------------------------------------------------------------------

#[test]
fn use_after_move() {
    err1(
        &main_with(" s := String::from(\"a\")\n take(s)\n take(s)"),
        "use of moved value: `s`",
    );
}

#[test]
fn use_after_move_has_note() {
    let src = main_with(" s := String::from(\"a\")\n t := s\n read(&s)");
    let e = check_source(&src);
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e[0].message, "borrow of moved value: `s`");
    assert!(
        e[0].notes
            .iter()
            .any(|(m, sp)| m == "value moved here" && sp.is_some()),
        "{:?}",
        e[0].notes
    );
    let r = e[0].render("t.kn", &src);
    assert!(r.contains("note: value moved here"), "{r}");
    assert!(r.contains("does not implement the `Copy` trait"), "{r}");
}

#[test]
fn copy_types_are_not_moved() {
    ok(&main_with(" n := 1\n m := n\n k := n + m\n b := true\n c := b\n r := &n\n r2 := r\n println(\"{r} {r2} {k} {c}\")"));
}

#[test]
fn derive_copy_struct_copies() {
    ok("#[derive(Copy, Clone)]\nstruct P {\n x: i32,\n}\nfn eat(p: P) {}\nfn main() {\n p := P { x: 1 }\n eat(p)\n eat(p)\n}");
    err1("struct P {\n x: i32,\n}\nfn eat(p: P) {}\nfn main() {\n p := P { x: 1 }\n eat(p)\n eat(p)\n}", "use of moved value: `p`");
}

#[test]
fn reassignment_reinitializes() {
    ok(&main_with(
        " s mut := String::from(\"a\")\n take(s)\n s = String::from(\"b\")\n take(s)",
    ));
}

#[test]
fn move_in_one_branch() {
    err1(
        &main_with(" s := String::from(\"a\")\n c := true\n if c {\n take(s)\n }\n read(&s)"),
        "borrow of moved value: `s`",
    );
    ok(&main_with(
        " s := String::from(\"a\")\n c := true\n if c {\n take(s)\n } else {\n read(&s)\n }",
    ));
}

#[test]
fn move_in_loop() {
    let e = check_source(&main_with(
        " s := String::from(\"a\")\n for i in 0..3 {\n take(s)\n }",
    ));
    assert_eq!(e.len(), 1, "{e:?}");
    assert!(
        e[0].notes
            .iter()
            .any(|(m, _)| m.contains("previous iteration")),
        "{:?}",
        e[0].notes
    );
}

#[test]
fn partial_moves() {
    let src = "struct P {\n a: String,\n b: String,\n}\nfn take(s: String) {}\nfn eat(p: P) {}\nfn main() {\n";
    ok(&format!("{src} p := P {{ a: String::from(\"x\"), b: String::from(\"y\") }}\n take(p.a)\n take(p.b)\n}}"));
    err1(&format!("{src} p := P {{ a: String::from(\"x\"), b: String::from(\"y\") }}\n take(p.a)\n eat(p)\n}}"), "use of partially moved value: `p`");
    err1(&format!("{src} p := P {{ a: String::from(\"x\"), b: String::from(\"y\") }}\n take(p.a)\n take(p.a)\n}}"), "use of moved value: `p.a`");
}

#[test]
fn move_out_of_reference() {
    err1(
        "struct P {\n a: String,\n}\nfn f(p: &P) -> String {\n return p.a\n}\nfn main() {}",
        "cannot move out of `*p.a`, which is behind a shared reference",
    );
    ok("struct P {\n a: String,\n}\nfn f(p: &P) -> String {\n return p.a.clone()\n}\nfn main() {}");
}

#[test]
fn move_out_of_index() {
    err1(
        "fn f(v: Vec<String>) -> String {\n return v[0]\n}\nfn main() {}",
        "cannot move out of index",
    );
    ok("fn f(v: Vec<i32>) -> i32 {\n return v[0]\n}\nfn main() {}");
}

#[test]
fn uninitialized_binding() {
    err1(
        "fn main() {\n let x: i32\n y := x + 1\n}",
        "used binding `x` isn't initialized",
    );
    ok("fn main() {\n let x: i32\n if true {\n x = 1\n } else {\n x = 2\n }\n y := x\n}");
    err1(
        "fn main() {\n let x: i32\n x = 1\n x = 2\n}",
        "cannot assign twice to immutable variable `x`",
    );
}

#[test]
fn moving_into_method_with_value_self() {
    err1(
        "fn main() {\n o := Some(String::from(\"a\"))\n a := o.unwrap()\n b := o.is_some()\n}",
        "borrow of moved value: `o`",
    );
}

#[test]
fn match_moves_and_ref_bindings() {
    let e = "enum E {\n A(String),\n B,\n}\nfn take(s: String) {}\n";
    // Matching through a reference binds by reference: no move.
    ok(&format!("{e}fn f(e: &E) {{\n match e {{\n E::A(s) => println(s),\n E::B => {{}},\n }}\n}}\nfn main() {{}}"));
    // Matching by value moves the payload out.
    has(&format!("{e}fn main() {{\n e := E::A(String::from(\"a\"))\n match e {{\n E::A(s) => take(s),\n E::B => {{}},\n }}\n match e {{\n E::A(s) => take(s),\n E::B => {{}},\n }}\n}}"), "moved value");
}

// ----- borrows ------------------------------------------------------------------------------

#[test]
fn shared_and_mutable_conflict() {
    err1(
        &main_with(" s mut := String::from(\"a\")\n r := &s\n m := &mut s\n read(r)"),
        "cannot borrow `s` as mutable because it is also borrowed as immutable",
    );
}

#[test]
fn two_mutable_borrows() {
    err1(
        &main_with(" s mut := String::from(\"a\")\n a := &mut s\n b := &mut s\n a.push(\"x\")"),
        "cannot borrow `s` as mutable more than once at a time",
    );
}

#[test]
fn non_lexical_lifetimes() {
    // The shared borrow's last use is before the mutable borrow.
    ok(&main_with(" s mut := String::from(\"a\")\n r := &s\n read(r)\n m := &mut s\n m.push(\"x\")\n read(&s)"));
    ok(&main_with(
        " s mut := String::from(\"a\")\n a := &mut s\n a.push(\"x\")\n b := &mut s\n b.push(\"y\")",
    ));
}

#[test]
fn use_while_mutably_borrowed() {
    err1(
        &main_with(" s mut := String::from(\"a\")\n m := &mut s\n read(&s)\n m.push(\"x\")"),
        "cannot borrow `s` as immutable because it is also borrowed as mutable",
    );
    err1(
        &main_with(" n mut := 1\n m := &mut n\n k := n + 1\n *m = 3"),
        "cannot use `n` because it was mutably borrowed",
    );
}

#[test]
fn assign_while_borrowed() {
    err1(
        &main_with(" n mut := 1\n r := &n\n n = 2\n println(\"{r}\")"),
        "cannot assign to `n` because it is borrowed",
    );
    ok(&main_with(
        " n mut := 1\n r := &n\n println(\"{r}\")\n n = 2",
    ));
}

#[test]
fn move_while_borrowed() {
    err1(
        &main_with(" s := String::from(\"a\")\n r := &s\n take(s)\n read(r)"),
        "cannot move out of `s` because it is borrowed",
    );
}

#[test]
fn writing_through_mut_ref_is_fine() {
    ok(&main_with(
        " n mut := 1\n m := &mut n\n *m = 3\n *m += 1\n println(\"{n}\")",
    ));
}

#[test]
fn disjoint_fields() {
    let s = "struct P {\n a: String,\n b: String,\n}\nfn main() {\n p mut := P { a: String::from(\"x\"), b: String::from(\"y\") }\n";
    ok(&format!(
        "{s} x := &mut p.a\n y := &p.b\n x.push(\"!\")\n println(y)\n}}"
    ));
    err1(
        &format!("{s} x := &mut p.a\n y := &p\n x.push(\"!\")\n println(\"{{y.b}}\")\n}}"),
        "cannot borrow `p` as immutable",
    );
}

#[test]
fn two_phase_method_calls() {
    ok("fn main() {\n v mut := Vec::new()\n v.push(1)\n v.push(v.len())\n}");
}

#[test]
fn reborrows_through_references() {
    ok("fn bump(n: &mut i32) {\n *n += 1\n}\nfn main() {\n x mut := 0\n r := &mut x\n bump(r)\n bump(r)\n println(\"{x}\")\n}");
}

#[test]
fn iterating_while_mutating() {
    has(
        "fn main() {\n v mut := Vec::new()\n v.push(1)\n for x in &v {\n v.push(*x)\n }\n}",
        "cannot borrow `v` as mutable because it is also borrowed as immutable",
    );
}

#[test]
fn borrow_outlives_owner() {
    let e =
        check_source("fn main() {\n let r: &i32\n {\n x := 5\n r = &x\n }\n println(\"{r}\")\n}");
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e[0].message, "`x` does not live long enough");
    assert!(e[0]
        .notes
        .iter()
        .any(|(m, _)| m.contains("dropped here while still borrowed")));
    assert!(e[0]
        .notes
        .iter()
        .any(|(m, _)| m.contains("borrow later used here")));
}

#[test]
fn block_value_borrowing_local() {
    err1(
        "fn main() {\n r := {\n x := 5\n &x\n }\n println(\"{r}\")\n}",
        "`x` does not live long enough",
    );
}

#[test]
fn drop_order_with_destructors() {
    let s = "struct Guard<'a> {\n r: &'a i32,\n}\nimpl<'a> Drop for Guard<'a> {\n fn drop(&mut self) {}\n}\n";
    // `g` is declared first, so `x` is dropped first while `g` (whose destructor runs later) still borrows it.
    has(
        &format!("{s}fn main() {{\n let mut g: Guard\n x := 1\n g = Guard {{ r: &x }}\n}}"),
        "`x` does not live long enough",
    );
    ok(&format!(
        "{s}fn main() {{\n x := 1\n g := Guard {{ r: &x }}\n}}"
    ));
}

// ----- returning references ----------------------------------------------------------------

#[test]
fn return_reference_to_local() {
    err1(
        "fn f() -> &'static i32 {\n x := 5\n return &x\n}\nfn main() {}",
        "cannot return reference to local variable `x`",
    );
    err1(
        "fn f(s: String) -> &'static str {\n return s.as_str()\n}\nfn main() {}",
        "cannot return reference to function parameter `s`",
    );
}

#[test]
fn return_borrow_of_parameter() {
    ok("fn first(v: &Vec<i32>) -> &i32 {\n return &v[0]\n}\nfn main() {}");
    ok("struct P {\n name: String,\n}\nimpl P {\n fn name(&self) -> &str {\n return self.name.as_str()\n }\n}\nfn main() {}");
}

#[test]
fn missing_lifetime_specifier() {
    let e = errs("fn longest(a: &str, b: &str) -> &str {\n return a\n}\nfn main() {}");
    assert_eq!(e, vec!["missing lifetime specifier"]);
    err1(
        "fn make() -> &str {\n return \"x\"\n}\nfn main() {}",
        "missing lifetime specifier",
    );
    ok("fn make() -> &'static str {\n return \"x\"\n}\nfn main() {}");
}

#[test]
fn explicit_lifetimes() {
    ok("fn longest<'a>(a: &'a str, b: &'a str) -> &'a str {\n if a.len() > b.len() {\n return a\n }\n return b\n}\nfn main() {}");
    err1(
        "fn first<'a, 'b>(a: &'a str, b: &'b str) -> &'a str {\n return b\n}\nfn main() {}",
        "lifetime may not live long enough: the returned value borrows from `b`",
    );
    err1(
        "fn f(a: &'x str) {}\nfn main() {}",
        "use of undeclared lifetime name `'x`",
    );
}

#[test]
fn self_elision() {
    err1(
        "struct P {\n name: String,\n}\nimpl P {\n fn pick(&self, other: &str) -> &str {\n return other\n }\n}\nfn main() {}",
        "lifetime may not live long enough: the returned value borrows from `other`",
    );
}

#[test]
fn borrowed_result_keeps_argument_borrowed() {
    err1(
        "fn first(v: &Vec<String>) -> &String {\n return &v[0]\n}\nfn main() {\n v mut := Vec::new()\n v.push(String::from(\"a\"))\n f := first(&v)\n v.push(String::from(\"b\"))\n println(f)\n}",
        "cannot borrow `v` as mutable because it is also borrowed as immutable",
    );
}

#[test]
fn structs_with_references() {
    ok("struct Parser<'src> {\n input: &'src str,\n pos: usize,\n}\nfn main() {\n text := String::from(\"abc\")\n p := Parser { input: text.as_str(), pos: 0 }\n println(p.input)\n}");
    err1(
        "struct Parser {\n input: &str,\n}\nfn main() {}",
        "missing lifetime specifier",
    );
    has(
        "struct View<'a> {\n s: &'a str,\n}\nfn main() {\n let v: View\n {\n t := String::from(\"x\")\n v = View { s: t.as_str() }\n }\n println(v.s)\n}",
        "`t` does not live long enough",
    );
}

// ----- defer ---------------------------------------------------------------------------------

#[test]
fn defer_sees_values_at_exit() {
    ok(&main_with(
        " s := String::from(\"a\")\n defer read(&s)\n println(\"working\")",
    ));
    err1(
        &main_with(" s := String::from(\"a\")\n defer read(&s)\n take(s)"),
        "borrow of moved value: `s`",
    );
}

// ----- closures ------------------------------------------------------------------------------

#[test]
fn closure_captures_by_reference() {
    ok(&main_with(
        " s := String::from(\"a\")\n f := || println(s)\n f()\n read(&s)",
    ));
    err1(
        &main_with(" s mut := String::from(\"a\")\n f := || read(&s)\n s.push(\"x\")\n f()"),
        "cannot borrow `s` as mutable because it is also borrowed as immutable",
    );
}

#[test]
fn closure_captures_by_mut_reference() {
    ok("fn main() {\n n mut := 0\n inc := || {\n n += 1\n }\n inc()\n inc()\n println(\"{n}\")\n}");
    err1(
        "fn main() {\n n mut := 0\n inc := || {\n n += 1\n }\n println(\"{n}\")\n inc()\n}",
        "cannot use `n` because it was mutably borrowed",
    );
}

#[test]
fn closure_captures_by_move() {
    err1(
        &main_with(" s := String::from(\"a\")\n f := || take(s)\n read(&s)"),
        "borrow of moved value: `s`",
    );
    err1(
        &main_with(" s := String::from(\"a\")\n f := move || println(s)\n read(&s)"),
        "borrow of moved value: `s`",
    );
}

#[test]
fn closure_body_is_checked() {
    err1(
        "fn take(s: String) {}\nfn main() {\n f := |s: String| {\n take(s)\n take(s)\n }\n}",
        "use of moved value: `s`",
    );
}

// ----- go and tasks -----------------------------------------------------------------------------

#[test]
fn go_moves_arguments() {
    err1(
        &main_with(" s := String::from(\"a\")\n go take(s)\n read(&s)"),
        "borrow of moved value: `s`",
    );
}

#[test]
fn go_rejects_borrowed_arguments() {
    err1(
        &main_with(" s := String::from(\"a\")\n go read(&s)"),
        "argument to `go` borrows `s`",
    );
    err1(
        "fn read(s: &String) {}\nfn f(s: &String) {\n go read(s)\n}\nfn main() {}",
        "borrows parameter `s`",
    );
    ok("fn say(s: &str) {}\nfn main() {\n go say(\"literal\")\n}");
}

#[test]
fn go_block_needs_move() {
    err1(
        &main_with(" s := String::from(\"a\")\n go {\n read(&s)\n }"),
        "`go` block may outlive the current function, but it borrows `s`",
    );
    ok(&main_with(
        " s := String::from(\"a\")\n go move {\n read(&s)\n }",
    ));
    err1(
        &main_with(" s := String::from(\"a\")\n go move {\n read(&s)\n }\n read(&s)"),
        "borrow of moved value: `s`",
    );
}

#[test]
fn go_requires_send() {
    let s = "struct Handle {\n p: *const u8,\n}\nfn use_it(h: Handle) {}\n";
    err1(
        &format!("{s}fn main() {{\n h := Handle {{ p: 0 as *const u8 }}\n go use_it(h)\n}}"),
        "`Handle` cannot be sent between tasks safely",
    );
    err1(&format!("{s}fn main() {{\n h := Handle {{ p: 0 as *const u8 }}\n go move {{\n use_it(h)\n }}\n}}"), "cannot be sent between tasks safely");
    ok(&format!("{s}unsafe impl Send for Handle {{}}\nfn main() {{\n h := Handle {{ p: 0 as *const u8 }}\n go use_it(h)\n}}"));
}

#[test]
fn send_is_structural() {
    let s = "struct Inner {\n p: *const u8,\n}\nstruct Outer {\n name: String,\n inner: Inner,\n}\nfn use_it(o: Outer) {}\n";
    has(&format!("{s}fn main() {{\n o := Outer {{ name: String::from(\"a\"), inner: Inner {{ p: 0 as *const u8 }} }}\n go use_it(o)\n}}"), "`Outer` cannot be sent");
}

#[test]
fn channels_of_owned_values_are_fine() {
    ok("fn worker(rx: <-chan String, tx: chan<- usize) {\n for s in rx {\n tx <- s.len()\n }\n}\nfn main() {\n (tx, rx) := chan::<String>(1)\n (otx, orx) := chan::<usize>(1)\n go worker(rx, otx)\n tx <- String::from(\"a\")\n n := <-orx\n}");
}

#[test]
fn scoped_tasks_may_borrow() {
    ok("fn main() {\n data := Vec::<i32>::new()\n scope(|s| {\n s.spawn(|| {\n println(\"{data.len()}\")\n })\n })\n println(\"{data.len()}\")\n}");
}

#[test]
fn scoped_tasks_check_sync() {
    let s = "struct Handle {\n p: *const u8,\n}\n";
    has(&format!("{s}fn main() {{\n h := Handle {{ p: 0 as *const u8 }}\n scope(|s| {{\n s.spawn(|| {{\n x := &h\n }})\n }})\n}}"), "`Handle` cannot be shared between tasks safely");
}

// ----- whole programs -----------------------------------------------------------------------------

#[test]
fn repository_examples_check() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "kn") {
            let src = std::fs::read_to_string(&path).unwrap();
            let e = check_source(&src);
            assert!(
                e.is_empty(),
                "{}:\n{}",
                path.display(),
                e.iter().map(|d| d.render("x.kn", &src)).collect::<String>()
            );
        }
    }
}

/// Every ```kiln block in the website must pass `kiln check` (use ```text for
/// intentionally broken code).
#[test]
fn website_examples_check() {
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
            let e = check_source(code);
            assert!(
                e.is_empty(),
                "{} block {}:\n{}",
                f.display(),
                i + 1,
                e.iter().map(|d| d.render("x.kn", code)).collect::<String>()
            );
            checked += 1;
        }
    }
    assert!(checked > 20, "only {checked} website examples found");
}

#[test]
fn send_and_sync_impls_must_be_unsafe() {
    let s = "struct Handle {\n p: *const u8,\n}\n";
    err1(
        &format!("{s}impl Send for Handle {{}}\nfn main() {{}}"),
        "the trait `Send` requires an `unsafe impl` declaration",
    );
    err1(
        &format!("{s}impl Sync for Handle {{}}\nfn main() {{}}"),
        "requires an `unsafe impl`",
    );
    ok(&format!(
        "{s}unsafe impl Sync for Handle {{}}\nfn main() {{}}"
    ));
    err1(
        &format!("{s}trait T {{}}\nunsafe impl T for Handle {{}}\nfn main() {{}}"),
        "implementing the trait `T` is not unsafe",
    );
}

#[test]
fn lifetime_bounds() {
    // `'b: 'a` lets data borrowed for `'b` be returned as `'a`.
    ok("fn pick<'a, 'b: 'a>(x: &'a str, y: &'b str) -> &'a str {\n return y\n}\nfn main() {}");
    ok("fn pick<'a, 'b>(x: &'a str, y: &'b str) -> &'a str where 'b: 'a {\n return y\n}\nfn main() {}");
    // Transitive.
    ok("fn pick<'a, 'b: 'c, 'c: 'a>(x: &'a str, y: &'b str) -> &'a str {\n return y\n}\nfn main() {}");
    err1(
        "fn pick<'a, 'b>(x: &'a str, y: &'b str) -> &'a str {\n return y\n}\nfn main() {}",
        "lifetime may not live long enough",
    );
    err1(
        "fn pick<'a>(x: &'a str) -> &'a str where 'a: 'q {\n return x\n}\nfn main() {}",
        "use of undeclared lifetime name `'q`",
    );
}

#[test]
fn trait_object_lifetimes() {
    let s = "trait Show {\n fn show(&self) -> String\n}\nstruct Wrap<'a> {\n s: &'a str,\n}\nimpl<'a> Show for Wrap<'a> {\n fn show(&self) -> String {\n return String::from(self.s)\n }\n}\n";
    // `Box<dyn Show>` means `Box<dyn Show + 'static>`: it can't hold a borrow of `s`.
    err1(&format!("{s}fn make(s: &str) -> Box<dyn Show> {{\n return Box::new(Wrap {{ s: s }})\n}}\nfn main() {{}}"), "lifetime may not live long enough");
    ok(&format!("{s}fn make<'a>(s: &'a str) -> Box<dyn Show + 'a> {{\n return Box::new(Wrap {{ s: s }})\n}}\nfn main() {{}}"));
}

#[test]
fn reassigned_reference_drops_old_borrow() {
    // After `r = &b`, `r` no longer borrows `a`, so `a` may be mutated.
    ok("fn main() {\n a mut := 1\n b := 2\n r mut := &a\n println(\"{r}\")\n r = &b\n a = 5\n println(\"{r} {a}\")\n}");
    // ...but while `r` may still point at `a` (one branch), it is borrowed.
    has("fn main() {\n a mut := 1\n b := 2\n r mut := &a\n if b > 1 {\n r = &b\n }\n a = 5\n println(\"{r}\")\n}", "cannot assign to `a` because it is borrowed");
}

#[test]
fn closures_that_move_captures_run_once() {
    err1(
        &main_with(" s := String::from(\"a\")\n f := || take(s)\n f()\n f()"),
        "use of moved value: `f`",
    );
    ok(&main_with(
        " s := String::from(\"a\")\n f := || take(s)\n f()",
    ));
    ok(&main_with(
        " s := String::from(\"a\")\n f := || read(&s)\n f()\n f()",
    ));
}

#[test]
fn defer_per_execution() {
    // A defer in a loop registers one call per iteration; all of them run at exit.
    err1(
        &main_with(" s := String::from(\"a\")\n for i in 0..3 {\n defer take(s)\n }"),
        "use of moved value: `s`",
    );
    ok(&main_with(
        " s := String::from(\"a\")\n for i in 0..3 {\n defer read(&s)\n }",
    ));
    // A conditional defer may or may not run.
    ok(&main_with(
        " s := String::from(\"a\")\n if true {\n defer take(s)\n }",
    ));
    // Deferred calls can't use variables from scopes that ended before the function returns.
    err1(
        &main_with(" {\n t := String::from(\"a\")\n defer read(&t)\n }"),
        "`t` is dropped before this deferred call runs",
    );
}

#[test]
fn defer_mir_shape() {
    let src = "fn main() {\n for i in 0..2 {\n defer println(\"x\")\n }\n}";
    let p = kiln_parser::parse(src);
    let pre = kiln_resolve::parse_prelude();
    let r = kiln_resolve::resolve(&p.file, &pre);
    let mut t = kiln_typeck::typeck(&r);
    let b = &kiln_mir::lower_all(&mut t)[0].1[0];
    let d = b.to_string();
    assert!(d.contains("defer_push 0"), "{d}");
    assert!(d.contains("defer_dispatch [bb"), "{d}");
}
