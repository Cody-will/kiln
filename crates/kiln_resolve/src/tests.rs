//! Unit tests for name resolution.

use crate::*;

fn errors(src: &str) -> Vec<String> {
    let p = kiln_parser::parse(src);
    assert!(p.errors.is_empty(), "parse errors: {:?}", p.errors);
    let prelude = parse_prelude();
    resolve(&p.file, &prelude)
        .errors
        .into_iter()
        .map(|e| e.message)
        .collect()
}

fn ok(src: &str) {
    let e = errors(src);
    assert!(e.is_empty(), "unexpected errors: {e:?}");
}

#[test]
fn prelude_resolves_cleanly() {
    ok("");
}

#[test]
fn locals_and_shadowing() {
    ok("fun main() {\n\tx := 1\n\tx := x + 1\n\tlet y = x\n\tprintln(\"{y}\")\n}");
}

#[test]
fn undefined_value() {
    assert_eq!(
        errors("fun main() {\n\tprintln(\"{z}\")\n}"),
        vec!["cannot find value `z` in this scope"]
    );
}

#[test]
fn use_before_declaration_is_an_error() {
    assert_eq!(
        errors("fun main() {\n\ty := x\n\tx := 1\n}"),
        vec!["cannot find value `x` in this scope"]
    );
}

#[test]
fn short_decl_initializer_cannot_see_its_own_name() {
    assert_eq!(
        errors("fun main() {\n\tx := x\n}"),
        vec!["cannot find value `x` in this scope"]
    );
}

#[test]
fn block_scopes_end() {
    assert_eq!(
        errors("fun main() {\n\tif true {\n\t\tinner := 1\n\t}\n\tprintln(\"{inner}\")\n}"),
        vec!["cannot find value `inner` in this scope"]
    );
}

#[test]
fn items_are_visible_before_definition() {
    ok("fun main() {\n\thelper()\n}\nfun helper() {}");
}

#[test]
fn undefined_type() {
    assert_eq!(
        errors("fun f(x Foo) {}"),
        vec!["cannot find type `Foo` in this scope"]
    );
}

#[test]
fn primitive_and_prelude_types() {
    ok("fun f(a i32, b &str, c option<string>, d result<Vec<u8>, string>) {}");
}

#[test]
fn generic_params_are_types() {
    ok("fun id<T>(x T) T {\n\treturn x\n}");
    assert_eq!(
        errors("fun id<T>(x U) {}"),
        vec!["cannot find type `U` in this scope"]
    );
}

#[test]
fn duplicate_definitions() {
    assert_eq!(
        errors("fun a() {}\nfun a() {}"),
        vec!["the name `a` is defined multiple times in this module (value namespace)"]
    );
    assert_eq!(
        errors("struct S\nenum S {}"),
        vec!["the name `S` is defined multiple times in this module (type namespace)"]
    );
    // A struct and a function may share a name only if the struct has named fields.
    ok("struct P {\n\tx i32,\n}\nfun P() {}");
}

#[test]
fn duplicate_fields_variants_and_bindings() {
    assert_eq!(
        errors("struct S {\n\tx i32,\n\tx i32,\n}"),
        vec!["field `x` is already declared"]
    );
    assert_eq!(
        errors("enum E {\n\tA,\n\tA,\n}"),
        vec!["variant `A` is defined multiple times"]
    );
    assert_eq!(
        errors("fun f(a i32, a i32) {}"),
        vec!["identifier `a` is bound more than once in the same pattern"]
    );
    assert_eq!(
        errors("fun main() {\n\t(a, a) := (1, 2)\n}"),
        vec!["identifier `a` is bound more than once in this declaration"]
    );
}

#[test]
fn enum_variants() {
    ok("enum Shape {\n\tCircle(f64),\n\tEmpty,\n}\nfun f(s Shape) {\n\tx := Shape::Circle(1.0)\n\tmatch s {\n\t\tShape::Circle(r) => {},\n\t\tShape::Empty => {},\n\t}\n}");
    // `E::B` may name an associated function, so the type checker reports
    // a missing one; deeper paths through a missing variant fail here.
    assert_eq!(
        errors("enum E {\n\tA,\n}\nfun f() {\n\tx := E::B::C\n}"),
        vec!["no variant named `B` in enum `E`"]
    );
}

#[test]
fn prelude_variants_are_in_scope() {
    ok("fun f(o option<i32>) result<i32, string> {\n\tmatch o {\n\t\tSome(x) => {\n\t\t\treturn Ok(x)\n\t\t},\n\t\tNone => {\n\t\t\treturn Err(string::from(\"none\"))\n\t\t},\n\t}\n}");
}

#[test]
fn unit_variant_pattern_is_not_a_binding() {
    let src = "fun f(o option<i32>) {\n\tmatch o {\n\t\tNone => {},\n\t\tother => {},\n\t}\n}";
    let p = kiln_parser::parse(src);
    let prelude = parse_prelude();
    let r = resolve(&p.file, &prelude);
    assert!(r.errors.is_empty());
    let names: Vec<_> = r
        .bindings
        .values()
        .map(|id| r.local(*id).name.clone())
        .collect();
    assert!(names.contains(&"other".to_string()));
    assert!(!names.contains(&"None".to_string()));
}

#[test]
fn self_in_methods_only() {
    ok("struct P {\n\tx i32,\n}\nimpl P {\n\tfun get(&self) i32 {\n\t\treturn self.x\n\t}\n\tfun new() Self {\n\t\treturn Self { x: 0 }\n\t}\n}");
    assert_eq!(
        errors("fun f() {\n\tx := self\n}"),
        vec!["`self` is only available in methods"]
    );
    assert_eq!(
        errors("fun f() Self {}"),
        vec!["cannot find type `Self` in this scope"]
    );
    assert_eq!(
        errors("fun f(&self) {}"),
        vec!["`self` parameter is only allowed in methods"]
    );
}

#[test]
fn associated_paths() {
    let src = "struct P\nimpl P {\n\tfun new() P {\n\t\treturn P\n\t}\n}\nfun main() {\n\tp := P::new()\n}";
    let p = kiln_parser::parse(src);
    let prelude = parse_prelude();
    let r = resolve(&p.file, &prelude);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(r
        .paths
        .values()
        .any(|res| matches!(res, Res::Assoc { name, .. } if name == "new")));
}

#[test]
fn inline_modules_and_visibility() {
    ok("mod util {\n\tpub fun helper() {}\n}\nfun main() {\n\tutil::helper()\n}");
    assert_eq!(
        errors("mod util {\n\tfun secret() {}\n}\nfun main() {\n\tutil::secret()\n}"),
        vec!["`secret` is private"]
    );
    // A child module can see its parent's private items via imports.
    ok("fun top() {}\nmod inner {\n\timport top\n\tfun f() {\n\t\ttop()\n\t}\n}");
}

#[test]
fn imports() {
    ok("mod shapes {\n\tpub struct Circle\n\tpub fun area() {}\n}\nimport shapes::{Circle, area as compute}\nfun main() {\n\tc := Circle\n\tcompute()\n}");
    ok("mod m {\n\tpub fun a() {}\n\tpub fun b() {}\n}\nimport m::*\nfun main() {\n\ta()\n\tb()\n}");
    assert_eq!(
        errors("import nothing::here"),
        vec!["unresolved import `nothing::here`"]
    );
}

#[test]
fn std_imports_are_external() {
    ok("import std::net::{TcpListener, TcpStream}\nimport std::io::{self, BufReader}\nfun f(c TcpStream) result<void, io::Error> {\n\tl := TcpListener::bind(\"x\")\n\treturn Ok()\n}");
}

#[test]
fn out_of_line_modules_not_supported() {
    assert_eq!(
        errors("mod net"),
        vec!["module `net` is declared without a body, but no file was loaded for it (build the package, or write `mod net { ... }`)"]
    );
}

#[test]
fn break_and_continue_need_loops() {
    assert_eq!(
        errors("fun f() {\n\tbreak\n}"),
        vec!["`break` outside of a loop"]
    );
    assert_eq!(
        errors("fun f() {\n\tloop {\n\t\tg := || {\n\t\t\tcontinue\n\t\t}\n\t}\n}"),
        vec!["`continue` outside of a loop"]
    );
    ok("fun f() {\n\t'outer: for i in 0..3 {\n\t\tloop {\n\t\t\tbreak 'outer\n\t\t}\n\t}\n}");
    assert_eq!(
        errors("fun f() {\n\tloop {\n\t\tbreak 'nope\n\t}\n}"),
        vec!["use of undeclared label `'nope`"]
    );
}

#[test]
fn trait_and_impl_paths() {
    ok("trait Describe {\n\tfun describe(&self) string\n}\nstruct P\nimpl Describe for P {\n\tfun describe(&self) string {\n\t\treturn string::new()\n\t}\n}\nfun show<T: Describe>(x &T) {}\nfun dynamic(x &dyn Describe) {}");
    assert_eq!(
        errors("struct NotATrait\nstruct P\nimpl NotATrait for P {}"),
        vec!["expected trait, found struct `NotATrait`"]
    );
}

#[test]
fn closures_bind_params() {
    ok("fun main() {\n\tadd := |a, b| a + b\n\tn := add(1, 2)\n}");
    assert_eq!(
        errors("fun main() {\n\tf := |a| b\n}"),
        vec!["cannot find value `b` in this scope"]
    );
}

#[test]
fn match_and_if_let_scopes() {
    ok("fun f(o option<i32>) {\n\tif let Some(v) = o {\n\t\tprintln(\"{v}\")\n\t}\n\tmatch o {\n\t\tSome(n) if n > 0 => println(\"{n}\"),\n\t\t_ => {},\n\t}\n}");
}

#[test]
fn or_patterns_must_bind_same_names() {
    assert_eq!(
        errors("enum E {\n\tA(i32),\n\tB(i32),\n\tC,\n}\nfun f(e E) {\n\tmatch e {\n\t\tE::A(x) | E::C => {},\n\t\t_ => {},\n\t}\n}"),
        vec!["all alternatives of an `|` pattern must bind the same names"]
    );
}

#[test]
fn select_binds_received_value() {
    ok("fun f(rx <-chan i32) {\n\tselect {\n\t\tv := <-rx => println(\"{v}\"),\n\t}\n}");
}

#[test]
fn chan_builtin_and_drop() {
    ok("fun main() {\n\t(tx, rx) := chan::<i32>(1)\n\tdrop(tx)\n}");
}

#[test]
fn nested_items_in_blocks() {
    ok("fun main() {\n\tfun helper() i32 {\n\t\treturn 1\n\t}\n\tx := helper()\n}");
}

#[test]
fn value_used_as_type_and_type_as_value() {
    assert_eq!(
        errors("fun g() {}\nfun f(x g) {}"),
        vec!["cannot find type `g` in this scope"]
    );
    assert_eq!(
        errors("struct P {\n\tx i32,\n}\nfun f() {\n\tp := P\n}"),
        vec!["cannot find value `P` in this scope"]
    );
}

#[test]
fn examples_resolve() {
    for src in [
        include_str!("../../../examples/hello.kn"),
        include_str!("../../../examples/shapes.kn"),
    ] {
        ok(src);
    }
}

#[test]
fn import_visibility() {
    let base = "mod a {\n\tpub struct S {}\n\tpub fun f() {}\n}\nmod b {\n\timport super::a::S\n\tpub import super::a::f\n\tfun g() {\n\t\tx := S {}\n\t}\n}\n";
    // `pub import` re-exports; the private import is usable inside `b`.
    ok(&format!(
        "{base}import b::f\nfun main() {{\n\tf()\n\tb::f()\n}}"
    ));
    assert_eq!(
        errors(&format!("{base}fun main() {{\n\tx := b::S {{}}\n}}")),
        vec!["`S` is imported privately in module `b`"]
    );
    assert_eq!(
        errors(&format!("{base}import b::S\nfun main() {{}}")),
        vec!["unresolved import `b::S`"]
    );
}
