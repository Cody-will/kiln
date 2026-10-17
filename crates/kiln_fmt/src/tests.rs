use super::format;

fn fmt(src: &str) -> String {
    match format(src) {
        Ok(s) => s,
        Err(e) => panic!("{e}: {e:?}\n{src}"),
    }
}

fn same(src: &str) {
    assert_eq!(fmt(src), src);
}

fn becomes(src: &str, want: &str) {
    let got = fmt(src);
    assert_eq!(got, want, "\n--- got:\n{got}\n--- want:\n{want}");
    assert_eq!(fmt(&got), got, "not idempotent");
}

#[test]
fn indentation_and_spacing() {
    becomes(
        "fun main(){\nx:=1+2*3\n    if x >3{\nprintln(\"{x}\")\n}\n}\n",
        "fun main() {\n\tx := 1 + 2 * 3\n\tif x > 3 {\n\t\tprintln(\"{x}\")\n\t}\n}\n",
    );
    becomes(
        "fun f(a i32,b &mut Vec<i32>)i32{\nreturn -a+ *b.first().unwrap()\n}\n",
        "fun f(a i32, b &mut Vec<i32>) i32 {\n\treturn -a + *b.first().unwrap()\n}\n",
    );
    same("fun main() {\n\tx := v[0]\n\ty := [1, 2, 3]\n\tz := !ok && x == 1\n\tv := <-ch\n\tch <- 5\n}\n");
}

#[test]
fn trailing_commas() {
    becomes(
        "struct P {\n\tx i32,\n\ty i32\n}\n\nfun main() {\n\tp := P { x: 1, y: 2, }\n\tf(\n\t\t1,\n\t\t2\n\t)\n}\n",
        "struct P {\n\tx i32,\n\ty i32,\n}\n\nfun main() {\n\tp := P { x: 1, y: 2 }\n\tf(\n\t\t1,\n\t\t2,\n\t)\n}\n",
    );
    // A one-element tuple keeps its comma; a parenthesized expression gets none.
    same("fun main() {\n\tt := (1,)\n\tx := (\n\t\t1 + 2\n\t)\n}\n");
    becomes(
        "fun main() {\n\tmatch x {\n\t\t1 => a(),\n\t\t_ => {\n\t\t\tb()\n\t\t}\n\t}\n}\n",
        "fun main() {\n\tmatch x {\n\t\t1 => a(),\n\t\t_ => {\n\t\t\tb()\n\t\t},\n\t}\n}\n",
    );
}

#[test]
fn comments_are_kept() {
    becomes(
        "//top\nfun main() { // trailing\n\t/* block */ x := 1\n\n\n\t// own line\n\ty := 2 //after\n}\n",
        "// top\nfun main() { // trailing\n\t/* block */ x := 1\n\n\t// own line\n\ty := 2 // after\n}\n",
    );
}

#[test]
fn blank_lines_and_semicolons() {
    becomes(
        "import std::io\nimport std::fmt\nimport std::io\nfun a() {\n\n\tx := 1;\n\n}\nfun b() {}\n",
        "import std::fmt\nimport std::io\n\nfun a() {\n\tx := 1\n}\n\nfun b() {}\n",
    );
    same("fun main() {\n\ta := [0 as u8; 16]\n}\n");
}

#[test]
fn continuation_lines() {
    same("fun main() {\n\tx := items\n\t\t.iter()\n\t\t.map(|v| {\n\t\t\treturn v + 1\n\t\t})\n\t\t.collect()\n\ty := a &&\n\t\tb\n}\n");
}

#[test]
fn glob_import_does_not_continue() {
    same("import a::*\nimport b::c\n\nfun main() {}\n");
}

#[test]
fn relaxed_bounds_pkg_visibility_and_generic_line_ends() {
    same("struct P<T: Ord + ?Sized, U: ?Sized> {\n\tpub(pkg) x T,\n}\n");
    same("type F<T> = fun mut(T) option<T>\ntype G = (i32,)\n");
    same("fun main() {\n\tx := a >\n\t\tb\n}\n");
}

#[test]
fn syntax_errors_are_reported() {
    assert!(format("fun main( {\n").is_err());
}

#[test]
fn mod_block_after_import_is_not_a_list() {
    same("import a::x\n\nmod inner {\n\tstruct A {}\n}\n");
}
