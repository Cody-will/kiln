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
    format!("fun take(s string) {{}}\nfun read(s &string) {{}}\nfun main() {{\n{body}\n}}")
}

// ----- moves -------------------------------------------------------------------------------

#[test]
fn use_after_move() {
    err1(
        &main_with(" s := string::from(\"a\")\n take(s)\n take(s)"),
        "use of moved value: `s`",
    );
}

#[test]
fn use_after_move_has_note() {
    let src = main_with(" s := string::from(\"a\")\n t := s\n read(&s)");
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
    ok("@derive(Copy, Clone)\nstruct P {\n x i32,\n}\nfun eat(p P) {}\nfun main() {\n p := P { x: 1 }\n eat(p)\n eat(p)\n}");
    err1("struct P {\n x i32,\n}\nfun eat(p P) {}\nfun main() {\n p := P { x: 1 }\n eat(p)\n eat(p)\n}", "use of moved value: `p`");
}

#[test]
fn reassignment_reinitializes() {
    ok(&main_with(
        " s mut := string::from(\"a\")\n take(s)\n s = string::from(\"b\")\n take(s)",
    ));
}

#[test]
fn move_in_one_branch() {
    err1(
        &main_with(" s := string::from(\"a\")\n c := true\n if c {\n take(s)\n }\n read(&s)"),
        "borrow of moved value: `s`",
    );
    ok(&main_with(
        " s := string::from(\"a\")\n c := true\n if c {\n take(s)\n } else {\n read(&s)\n }",
    ));
}

#[test]
fn move_in_loop() {
    let e = check_source(&main_with(
        " s := string::from(\"a\")\n for i in 0..3 {\n take(s)\n }",
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
    let src = "struct P {\n a string,\n b string,\n}\nfun take(s string) {}\nfun eat(p P) {}\nfun main() {\n";
    ok(&format!("{src} p := P {{ a: string::from(\"x\"), b: string::from(\"y\") }}\n take(p.a)\n take(p.b)\n}}"));
    err1(&format!("{src} p := P {{ a: string::from(\"x\"), b: string::from(\"y\") }}\n take(p.a)\n eat(p)\n}}"), "use of partially moved value: `p`");
    err1(&format!("{src} p := P {{ a: string::from(\"x\"), b: string::from(\"y\") }}\n take(p.a)\n take(p.a)\n}}"), "use of moved value: `p.a`");
}

#[test]
fn move_out_of_reference() {
    err1(
        "struct P {\n a string,\n}\nfun f(p &P) string {\n return p.a\n}\nfun main() {}",
        "cannot move out of `*p.a`, which is behind a shared reference",
    );
    ok("struct P {\n a string,\n}\nfun f(p &P) string {\n return p.a.clone()\n}\nfun main() {}");
}

#[test]
fn move_out_of_index() {
    err1(
        "fun f(v Vec<string>) string {\n return v[0]\n}\nfun main() {}",
        "cannot move out of index",
    );
    ok("fun f(v Vec<i32>) i32 {\n return v[0]\n}\nfun main() {}");
}

#[test]
fn uninitialized_binding() {
    err1(
        "fun main() {\n let x i32\n y := x + 1\n}",
        "used binding `x` isn't initialized",
    );
    ok("fun main() {\n let x i32\n if true {\n x = 1\n } else {\n x = 2\n }\n y := x\n}");
    err1(
        "fun main() {\n let x i32\n x = 1\n x = 2\n}",
        "cannot assign twice to immutable variable `x`",
    );
}

#[test]
fn moving_into_method_with_value_self() {
    err1(
        "fun main() {\n o := Some(string::from(\"a\"))\n a := o.unwrap()\n b := o.is_some()\n}",
        "borrow of moved value: `o`",
    );
}

#[test]
fn match_moves_and_ref_bindings() {
    let e = "enum E {\n A(string),\n B,\n}\nfun take(s string) {}\n";
    // Matching through a reference binds by reference: no move.
    ok(&format!("{e}fun f(e &E) {{\n match e {{\n E::A(s) => println(s),\n E::B => {{}},\n }}\n}}\nfun main() {{}}"));
    // Matching by value moves the payload out.
    has(&format!("{e}fun main() {{\n e := E::A(string::from(\"a\"))\n match e {{\n E::A(s) => take(s),\n E::B => {{}},\n }}\n match e {{\n E::A(s) => take(s),\n E::B => {{}},\n }}\n}}"), "moved value");
}

// ----- borrows ------------------------------------------------------------------------------

#[test]
fn shared_and_mutable_conflict() {
    err1(
        &main_with(" s mut := string::from(\"a\")\n r := &s\n m := &mut s\n read(r)"),
        "cannot borrow `s` as mutable because it is also borrowed as immutable",
    );
}

#[test]
fn two_mutable_borrows() {
    err1(
        &main_with(" s mut := string::from(\"a\")\n a := &mut s\n b := &mut s\n a.push(\"x\")"),
        "cannot borrow `s` as mutable more than once at a time",
    );
}

#[test]
fn non_lexical_lifetimes() {
    // The shared borrow's last use is before the mutable borrow.
    ok(&main_with(" s mut := string::from(\"a\")\n r := &s\n read(r)\n m := &mut s\n m.push(\"x\")\n read(&s)"));
    ok(&main_with(
        " s mut := string::from(\"a\")\n a := &mut s\n a.push(\"x\")\n b := &mut s\n b.push(\"y\")",
    ));
}

#[test]
fn use_while_mutably_borrowed() {
    err1(
        &main_with(" s mut := string::from(\"a\")\n m := &mut s\n read(&s)\n m.push(\"x\")"),
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
        &main_with(" s := string::from(\"a\")\n r := &s\n take(s)\n read(r)"),
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
    let s = "struct P {\n a string,\n b string,\n}\nfun main() {\n p mut := P { a: string::from(\"x\"), b: string::from(\"y\") }\n";
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
    ok("fun main() {\n v mut := Vec::new()\n v.push(1)\n v.push(v.len())\n}");
}

#[test]
fn reborrows_through_references() {
    ok("fun bump(n &mut i32) {\n *n += 1\n}\nfun main() {\n x mut := 0\n r := &mut x\n bump(r)\n bump(r)\n println(\"{x}\")\n}");
}

#[test]
fn iterating_while_mutating() {
    has(
        "fun main() {\n v mut := Vec::new()\n v.push(1)\n for x in &v {\n v.push(*x)\n }\n}",
        "cannot borrow `v` as mutable because it is also borrowed as immutable",
    );
}

#[test]
fn borrow_outlives_owner() {
    let e =
        check_source("fun main() {\n let r &i32\n {\n x := 5\n r = &x\n }\n println(\"{r}\")\n}");
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
        "fun main() {\n r := {\n x := 5\n &x\n }\n println(\"{r}\")\n}",
        "`x` does not live long enough",
    );
}

#[test]
fn drop_order_with_destructors() {
    let s = "struct Guard<'a> {\n r &'a i32,\n}\nimpl<'a> Drop for Guard<'a> {\n fun drop(&mut self) {}\n}\n";
    // `g` is declared first, so `x` is dropped first while `g` (whose destructor runs later) still borrows it.
    has(
        &format!("{s}fun main() {{\n let mut g Guard\n x := 1\n g = Guard {{ r: &x }}\n}}"),
        "`x` does not live long enough",
    );
    ok(&format!(
        "{s}fun main() {{\n x := 1\n g := Guard {{ r: &x }}\n}}"
    ));
}

// ----- returning references ----------------------------------------------------------------

#[test]
fn return_reference_to_local() {
    err1(
        "fun f() &'static i32 {\n x := 5\n return &x\n}\nfun main() {}",
        "cannot return reference to local variable `x`",
    );
    err1(
        "fun f(s string) &'static str {\n return s.as_str()\n}\nfun main() {}",
        "cannot return reference to function parameter `s`",
    );
}

#[test]
fn return_borrow_of_parameter() {
    ok("fun first(v &Vec<i32>) &i32 {\n return &v[0]\n}\nfun main() {}");
    ok("struct P {\n name string,\n}\nimpl P {\n fun name(&self) &str {\n return self.name.as_str()\n }\n}\nfun main() {}");
}

#[test]
fn missing_lifetime_specifier() {
    let e = errs("fun longest(a &str, b &str) &str {\n return a\n}\nfun main() {}");
    assert_eq!(e, vec!["missing lifetime specifier"]);
    err1(
        "fun make() &str {\n return \"x\"\n}\nfun main() {}",
        "missing lifetime specifier",
    );
    ok("fun make() &'static str {\n return \"x\"\n}\nfun main() {}");
}

#[test]
fn explicit_lifetimes() {
    ok("fun longest<'a>(a &'a str, b &'a str) &'a str {\n if a.len() > b.len() {\n return a\n }\n return b\n}\nfun main() {}");
    err1(
        "fun first<'a, 'b>(a &'a str, b &'b str) &'a str {\n return b\n}\nfun main() {}",
        "lifetime may not live long enough: the returned value borrows from `b`",
    );
    err1(
        "fun f(a &'x str) {}\nfun main() {}",
        "use of undeclared lifetime name `'x`",
    );
}

#[test]
fn self_elision() {
    err1(
        "struct P {\n name string,\n}\nimpl P {\n fun pick(&self, other &str) &str {\n return other\n }\n}\nfun main() {}",
        "lifetime may not live long enough: the returned value borrows from `other`",
    );
}

#[test]
fn borrowed_result_keeps_argument_borrowed() {
    err1(
        "fun first(v &Vec<string>) &string {\n return &v[0]\n}\nfun main() {\n v mut := Vec::new()\n v.push(string::from(\"a\"))\n f := first(&v)\n v.push(string::from(\"b\"))\n println(f)\n}",
        "cannot borrow `v` as mutable because it is also borrowed as immutable",
    );
}

#[test]
fn structs_with_references() {
    ok("struct Parser<'src> {\n input &'src str,\n pos usize,\n}\nfun main() {\n text := string::from(\"abc\")\n p := Parser { input: text.as_str(), pos: 0 }\n println(p.input)\n}");
    err1(
        "struct Parser {\n input &str,\n}\nfun main() {}",
        "missing lifetime specifier",
    );
    has(
        "struct View<'a> {\n s &'a str,\n}\nfun main() {\n let v View\n {\n t := string::from(\"x\")\n v = View { s: t.as_str() }\n }\n println(v.s)\n}",
        "`t` does not live long enough",
    );
}

// ----- defer ---------------------------------------------------------------------------------

#[test]
fn defer_evaluates_arguments_at_the_defer_statement() {
    ok(&main_with(
        " s := string::from(\"a\")\n defer read(&s)\n println(\"working\")",
    ));
    // The deferred call keeps its borrow of `s` until the function returns.
    err1(
        &main_with(" s := string::from(\"a\")\n defer read(&s)\n take(s)"),
        "cannot move out of `s` because it is borrowed",
    );
    // A moved argument is moved at the `defer`, like any call.
    err1(
        &main_with(" s := string::from(\"a\")\n defer take(s)\n println(s)"),
        "use of moved value: `s`",
    );
    // Copy arguments are copied: later assignments don't affect the deferred call.
    ok("fun show(n i32) {\n}\nfun main() {\n x mut := 1\n defer show(x)\n x = 2\n show(x)\n}");
    // Printing an interpolated string formats it at the `defer`.
    ok("fun main() {\n for i in 0..3 {\n defer println(\"{i}\")\n }\n}");
}

#[test]
fn defer_blocks_see_values_at_exit() {
    // `defer { ... }` runs the block at exit, like a Go closure.
    err1(
        &main_with(" s := string::from(\"a\")\n defer {\n read(&s)\n }\n take(s)"),
        "borrow of moved value: `s`",
    );
    err1(
        &main_with(" {\n t := string::from(\"a\")\n defer {\n read(&t)\n }\n }"),
        "`t` is dropped before this deferred call runs",
    );
}

// ----- closures ------------------------------------------------------------------------------

#[test]
fn closure_captures_by_reference() {
    ok(&main_with(
        " s := string::from(\"a\")\n f := || println(s)\n f()\n read(&s)",
    ));
    err1(
        &main_with(" s mut := string::from(\"a\")\n f := || read(&s)\n s.push(\"x\")\n f()"),
        "cannot borrow `s` as mutable because it is also borrowed as immutable",
    );
}

#[test]
fn closure_captures_by_mut_reference() {
    ok("fun main() {\n n mut := 0\n inc := || {\n n += 1\n }\n inc()\n inc()\n println(\"{n}\")\n}");
    err1(
        "fun main() {\n n mut := 0\n inc := || {\n n += 1\n }\n println(\"{n}\")\n inc()\n}",
        "cannot use `n` because it was mutably borrowed",
    );
}

#[test]
fn closure_captures_by_move() {
    err1(
        &main_with(" s := string::from(\"a\")\n f := || take(s)\n read(&s)"),
        "borrow of moved value: `s`",
    );
    err1(
        &main_with(" s := string::from(\"a\")\n f := move || println(s)\n read(&s)"),
        "borrow of moved value: `s`",
    );
}

#[test]
fn closure_body_is_checked() {
    err1(
        "fun take(s string) {}\nfun main() {\n f := |s string| {\n take(s)\n take(s)\n }\n}",
        "use of moved value: `s`",
    );
}

// ----- go and tasks -----------------------------------------------------------------------------

#[test]
fn go_moves_arguments() {
    err1(
        &main_with(" s := string::from(\"a\")\n spawn take(s)\n read(&s)"),
        "borrow of moved value: `s`",
    );
}

#[test]
fn go_rejects_borrowed_arguments() {
    err1(
        &main_with(" s := string::from(\"a\")\n spawn read(&s)"),
        "argument to `spawn` borrows `s`",
    );
    err1(
        "fun read(s &string) {}\nfun f(s &string) {\n spawn read(s)\n}\nfun main() {}",
        "borrows parameter `s`",
    );
    ok("fun say(s &str) {}\nfun main() {\n spawn say(\"literal\")\n}");
}

#[test]
fn go_block_needs_move() {
    err1(
        &main_with(" s := string::from(\"a\")\n spawn {\n read(&s)\n }"),
        "`spawn` block may outlive the current function, but it borrows `s`",
    );
    ok(&main_with(
        " s := string::from(\"a\")\n spawn move {\n read(&s)\n }",
    ));
    err1(
        &main_with(" s := string::from(\"a\")\n spawn move {\n read(&s)\n }\n read(&s)"),
        "borrow of moved value: `s`",
    );
}

#[test]
fn go_requires_send() {
    let s = "struct Handle {\n p *const u8,\n}\nfun use_it(h Handle) {}\n";
    err1(
        &format!("{s}fun main() {{\n h := Handle {{ p: 0 as *const u8 }}\n spawn use_it(h)\n}}"),
        "`Handle` cannot be sent between tasks safely",
    );
    err1(&format!("{s}fun main() {{\n h := Handle {{ p: 0 as *const u8 }}\n spawn move {{\n use_it(h)\n }}\n}}"), "cannot be sent between tasks safely");
    ok(&format!("{s}unsafe impl Send for Handle {{}}\nfun main() {{\n h := Handle {{ p: 0 as *const u8 }}\n spawn use_it(h)\n}}"));
}

#[test]
fn send_is_structural() {
    let s = "struct Inner {\n p *const u8,\n}\nstruct Outer {\n name string,\n inner Inner,\n}\nfun use_it(o Outer) {}\n";
    has(&format!("{s}fun main() {{\n o := Outer {{ name: string::from(\"a\"), inner: Inner {{ p: 0 as *const u8 }} }}\n spawn use_it(o)\n}}"), "`Outer` cannot be sent");
}

#[test]
fn channels_of_owned_values_are_fine() {
    ok("fun worker(rx <-chan string, tx chan<- usize) {\n for s in rx {\n tx <- s.len()\n }\n}\nfun main() {\n (tx, rx) := chan::<string>(1)\n (otx, orx) := chan::<usize>(1)\n spawn worker(rx, otx)\n tx <- string::from(\"a\")\n n := <-orx\n}");
}

#[test]
fn scoped_tasks_may_borrow() {
    ok("fun main() {\n data := Vec::<i32>::new()\n scope(|s| {\n s.spawn(|| {\n println(\"{data.len()}\")\n })\n })\n println(\"{data.len()}\")\n}");
}

#[test]
fn scoped_tasks_check_sync() {
    let s = "struct Handle {\n p *const u8,\n}\n";
    has(&format!("{s}fun main() {{\n h := Handle {{ p: 0 as *const u8 }}\n scope(|s| {{\n s.spawn(|| {{\n x := &h\n }})\n }})\n}}"), "`Handle` cannot be shared between tasks safely");
}

#[test]
fn scoped_task_borrows_last_until_the_scope_ends() {
    // Two tasks may not mutate the same variable: both borrows last until the scope ends.
    has("fun main() {\n hits mut := 0\n scope(|s| {\n s.spawn(|| {\n hits += 1\n })\n s.spawn(|| {\n hits += 1\n })\n })\n}", "cannot borrow `hits` as mutable more than once at a time");
    // A task may not borrow a local of the scope body, which ends before the tasks are joined.
    has(
        "fun main() {\n scope(|s| {\n x := 5\n s.spawn(|| {\n println(\"{x}\")\n })\n })\n}",
        "`x` does not live long enough",
    );
    // Sharing is fine.
    ok("fun main() {\n n := 3\n scope(|s| {\n s.spawn(|| {\n println(\"{n}\")\n })\n s.spawn(|| {\n println(\"{n}\")\n })\n })\n}");
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
    let s = "struct Handle {\n p *const u8,\n}\n";
    err1(
        &format!("{s}impl Send for Handle {{}}\nfun main() {{}}"),
        "the trait `Send` requires an `unsafe impl` declaration",
    );
    err1(
        &format!("{s}impl Sync for Handle {{}}\nfun main() {{}}"),
        "requires an `unsafe impl`",
    );
    ok(&format!(
        "{s}unsafe impl Sync for Handle {{}}\nfun main() {{}}"
    ));
    err1(
        &format!("{s}trait T {{}}\nunsafe impl T for Handle {{}}\nfun main() {{}}"),
        "implementing the trait `T` is not unsafe",
    );
}

#[test]
fn lifetime_bounds() {
    // `'b: 'a` lets data borrowed for `'b` be returned as `'a`.
    ok("fun pick<'a, 'b: 'a>(x &'a str, y &'b str) &'a str {\n return y\n}\nfun main() {}");
    ok("fun pick<'a, 'b>(x &'a str, y &'b str) &'a str where 'b: 'a {\n return y\n}\nfun main() {}");
    // Transitive.
    ok("fun pick<'a, 'b: 'c, 'c: 'a>(x &'a str, y &'b str) &'a str {\n return y\n}\nfun main() {}");
    err1(
        "fun pick<'a, 'b>(x &'a str, y &'b str) &'a str {\n return y\n}\nfun main() {}",
        "lifetime may not live long enough",
    );
    err1(
        "fun pick<'a>(x &'a str) &'a str where 'a: 'q {\n return x\n}\nfun main() {}",
        "use of undeclared lifetime name `'q`",
    );
}

#[test]
fn trait_object_lifetimes() {
    let s = "trait Show {\n fun show(&self) string\n}\nstruct Wrap<'a> {\n s &'a str,\n}\nimpl<'a> Show for Wrap<'a> {\n fun show(&self) string {\n return string::from(self.s)\n }\n}\n";
    // `Box<dyn Show>` means `Box<dyn Show + 'static>`: it can't hold a borrow of `s`.
    err1(&format!("{s}fun make(s &str) Box<dyn Show> {{\n return Box::new(Wrap {{ s: s }})\n}}\nfun main() {{}}"), "lifetime may not live long enough");
    ok(&format!("{s}fun make<'a>(s &'a str) Box<dyn Show + 'a> {{\n return Box::new(Wrap {{ s: s }})\n}}\nfun main() {{}}"));
}

#[test]
fn reassigned_reference_drops_old_borrow() {
    // After `r = &b`, `r` no longer borrows `a`, so `a` may be mutated.
    ok("fun main() {\n a mut := 1\n b := 2\n r mut := &a\n println(\"{r}\")\n r = &b\n a = 5\n println(\"{r} {a}\")\n}");
    // ...but while `r` may still point at `a` (one branch), it is borrowed.
    has("fun main() {\n a mut := 1\n b := 2\n r mut := &a\n if b > 1 {\n r = &b\n }\n a = 5\n println(\"{r}\")\n}", "cannot assign to `a` because it is borrowed");
}

#[test]
fn closures_that_move_captures_run_once() {
    err1(
        &main_with(" s := string::from(\"a\")\n f := || take(s)\n f()\n f()"),
        "use of moved value: `f`",
    );
    ok(&main_with(
        " s := string::from(\"a\")\n f := || take(s)\n f()",
    ));
    ok(&main_with(
        " s := string::from(\"a\")\n f := || read(&s)\n f()\n f()",
    ));
}

#[test]
fn defer_per_execution() {
    // A defer in a loop registers one call per iteration; all of them run at exit.
    err1(
        &main_with(" s := string::from(\"a\")\n for i in 0..3 {\n defer take(s)\n }"),
        "use of moved value: `s`",
    );
    ok(&main_with(
        " s := string::from(\"a\")\n for i in 0..3 {\n defer read(&s)\n }",
    ));
    // A conditional defer may or may not run.
    ok(&main_with(
        " s := string::from(\"a\")\n if true {\n defer take(s)\n }",
    ));
    // A deferred call can't keep a borrow of a variable whose scope ends first.
    err1(
        &main_with(" {\n t := string::from(\"a\")\n defer read(&t)\n }"),
        "`t` does not live long enough",
    );
}

#[test]
fn defer_mir_shape() {
    let src = "fun main() {\n for i in 0..2 {\n defer println(\"x\")\n }\n}";
    let p = kiln_parser::parse(src);
    let pre = kiln_resolve::parse_prelude();
    let r = kiln_resolve::resolve(&p.file, &pre);
    let mut t = kiln_typeck::typeck(&r);
    let b = &kiln_mir::lower_all(&mut t)[0].1[0];
    let d = b.to_string();
    assert!(d.contains("defer_push 0"), "{d}");
    assert!(d.contains("defer_dispatch [bb"), "{d}");
}

/// The standard library (written in Kiln) passes the borrow checker.
#[test]
fn standard_library_borrow_checks() {
    let parsed = kiln_parser::parse("fun main() {}");
    let prelude = kiln_resolve::parse_prelude();
    let resolved = kiln_resolve::resolve(&parsed.file, &prelude);
    assert!(resolved.errors.is_empty());
    let mut typed = kiln_typeck::typeck(&resolved);
    assert!(typed.errors().is_empty());
    let errors = crate::check_std(&mut typed);
    let shown: Vec<String> = errors
        .iter()
        .map(|d| d.render("x.kn", "fun main() {}"))
        .collect();
    assert!(errors.is_empty(), "{}", shown.join("\n"));
}

#[test]
fn closure_capabilities() {
    // A closure that moves a capture out can't become a `fn` value (it could
    // be called twice).
    err1(
        "fun twice(f fun()) {\n f()\n f()\n}\nfun main() {\n s := string::from(\"x\")\n twice(move || {\n  t := s\n })\n}",
        "moves a captured value out",
    );
    // Mutating closures need `fn mut`; plain `fn` values are shared freely.
    err1(
        "fun each(f fun(i32)) {\n f(1)\n}\nfun main() {\n n mut := 0\n each(|x| {\n  n += x\n })\n}",
        "mutates the captured variable `n`",
    );
    ok("fun each(f fun mut(i32)) {\n f(1)\n f(2)\n}\nfun main() {\n n mut := 0\n each(|x| {\n  n += x\n })\n f := |x| {\n  n += x\n }\n f(3)\n}");
    // Captures must be `Send + Sync` for the closure to be a `fn` value.
    err1(
        "fun run(f fun()) {\n f()\n}\nfun main() {\n x mut := 1\n p := &mut x as *mut i32\n run(move || {\n  q := p\n })\n}",
        "which is not `Send` and `Sync`",
    );
    // `fn mut` values are not `Copy`, `Send`, or `Sync`.
    err1(
        "fun each(f fun mut(i32)) {\n g := f\n f(1)\n}\nfun main() {}",
        "use of moved value",
    );
    // A plain `fn` coerces to `fn mut`, not the other way around.
    ok("fun each(f fun mut(i32)) {\n f(1)\n}\nfun show(x i32) {}\nfun main() {\n each(show)\n}");
}

#[test]
fn deep_flows_through_generic_results() {
    // `take` returns a value from behind `&mut self`, not a borrow of it.
    ok("fun main() {\n o mut := Some(5)\n v := o.take()\n b := o.is_none()\n w := v\n}");
    ok("fun main() {\n v mut := Vec::new()\n v.push(1)\n old := std::mem::replace(&mut v, Vec::new())\n v.push(2)\n n := old.len()\n}");
    // But a reference moved out keeps its own loan.
    err1(
        "fun main() {\n x mut := 1\n slot mut := Some(&x)\n r := slot.take().unwrap()\n x = 3\n y := *r\n}",
        "cannot assign to `x` because it is borrowed",
    );
    err1(
        "fun main() {\n x mut := 1\n y := 2\n v mut := &x\n r := std::mem::replace(&mut v, &y)\n x = 3\n z := *r\n}",
        "cannot assign to `x` because it is borrowed",
    );
}

#[test]
fn reassigning_a_reference_keeps_borrows_through_it() {
    // Walking a linked structure: `cur` moves on while `&(*e).next` is live.
    ok("struct N {\n v i32,\n next option<Box<N>>,\n}\nfun sum(n &N) i32 {\n t mut := 0\n cur mut := Some(n)\n while let Some(e) = cur {\n  t += e.v\n  cur = match &e.next {\n   Some(b) => Some(&**b),\n   None => None,\n  }\n }\n return t\n}\nfun main() {}");
    // But overwriting an owner is still a conflict.
    err1(
        "fun main() {\n v mut := Vec::new()\n v.push(1)\n r := &v[0]\n v = Vec::new()\n x := *r\n}",
        "cannot assign to `v` because it is borrowed",
    );
}

#[test]
fn references_stored_in_structs_outlive_the_struct() {
    let ty = "struct S<'a> {\n r &'a str,\n owned string,\n}\n";
    // A `&'a` field read through `&self` lives as long as the original data.
    let good = format!("{ty}impl<'a> S<'a> {{\n fun head(&self) &'a str {{\n  return &self.r[0..1]\n }}\n fun all(&self) &'a str {{\n  return self.r\n }}\n}}\nfun main() {{\n text := \"hello\".to_string()\n x mut := \"\"\n {{\n  s := S {{ r: text.as_str(), owned: string::new() }}\n  x = s.head()\n }}\n println(x)\n}}");
    ok(&good);
    // But data the struct owns does not.
    err1(
        &format!("{ty}impl<'a> S<'a> {{\n fun bad(&self) &'a string {{\n  return &self.owned\n }}\n}}\nfun main() {{}}"),
        "lifetime may not live long enough",
    );
    // And the original data stays borrowed while the result is used.
    has(
        &format!("{ty}impl<'a> S<'a> {{\n fun all(&self) &'a str {{\n  return self.r\n }}\n}}\nfun main() {{\n text mut := \"hello\".to_string()\n s := S {{ r: text.as_str(), owned: string::new() }}\n x := s.all()\n text = string::new()\n println(x)\n}}"),
        "because it is borrowed",
    );
}
