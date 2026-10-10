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
        "fn main(){\nx:=1+2*3\n    if x >3{\nprintln(\"{x}\")\n}\n}\n",
        "fn main() {\n\tx := 1 + 2 * 3\n\tif x > 3 {\n\t\tprintln(\"{x}\")\n\t}\n}\n",
    );
    becomes(
        "fn f(a:i32,b:&mut Vec<i32>)->i32{\nreturn -a+ *b.first().unwrap()\n}\n",
        "fn f(a: i32, b: &mut Vec<i32>) -> i32 {\n\treturn -a + *b.first().unwrap()\n}\n",
    );
    same("fn main() {\n\tx := v[0]\n\ty := [1, 2, 3]\n\tz := !ok && x == 1\n\tv := <-ch\n\tch <- 5\n}\n");
}

#[test]
fn trailing_commas() {
    becomes(
        "struct P {\n\tx: i32,\n\ty: i32\n}\n\nfn main() {\n\tp := P { x: 1, y: 2, }\n\tf(\n\t\t1,\n\t\t2\n\t)\n}\n",
        "struct P {\n\tx: i32,\n\ty: i32,\n}\n\nfn main() {\n\tp := P { x: 1, y: 2 }\n\tf(\n\t\t1,\n\t\t2,\n\t)\n}\n",
    );
    // A one-element tuple keeps its comma; a parenthesized expression gets none.
    same("fn main() {\n\tt := (1,)\n\tx := (\n\t\t1 + 2\n\t)\n}\n");
    becomes(
        "fn main() {\n\tmatch x {\n\t\t1 => a(),\n\t\t_ => {\n\t\t\tb()\n\t\t}\n\t}\n}\n",
        "fn main() {\n\tmatch x {\n\t\t1 => a(),\n\t\t_ => {\n\t\t\tb()\n\t\t},\n\t}\n}\n",
    );
}

#[test]
fn comments_are_kept() {
    becomes(
        "//top\nfn main() { // trailing\n\t/* block */ x := 1\n\n\n\t// own line\n\ty := 2 //after\n}\n",
        "// top\nfn main() { // trailing\n\t/* block */ x := 1\n\n\t// own line\n\ty := 2 // after\n}\n",
    );
}

#[test]
fn blank_lines_and_semicolons() {
    becomes(
        "import std::io\nimport std::fmt\nimport std::io\nfn a() {\n\n\tx := 1;\n\n}\nfn b() {}\n",
        "import std::fmt\nimport std::io\n\nfn a() {\n\tx := 1\n}\n\nfn b() {}\n",
    );
    same("fn main() {\n\ta := [0 as u8; 16]\n}\n");
}

#[test]
fn continuation_lines() {
    same("fn main() {\n\tx := items\n\t\t.iter()\n\t\t.map(|v| {\n\t\t\treturn v + 1\n\t\t})\n\t\t.collect()\n\ty := a &&\n\t\tb\n}\n");
}

#[test]
fn syntax_errors_are_reported() {
    assert!(format("fn main( {\n").is_err());
}
