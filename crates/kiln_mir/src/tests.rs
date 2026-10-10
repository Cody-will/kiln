//! MIR lowering tests.

use crate::*;

fn lower(src: &str) -> Vec<Body> {
    let p = kiln_parser::parse(src);
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    let prelude = kiln_resolve::parse_prelude();
    let r = kiln_resolve::resolve(&p.file, &prelude);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let mut t = kiln_typeck::typeck(&r);
    assert!(t.errors().is_empty(), "{:?}", t.errors());
    lower_all(&mut t).into_iter().flat_map(|(_, b)| b).collect()
}

fn dump(src: &str) -> String {
    lower(src).iter().map(|b| b.to_string()).collect()
}

#[test]
fn moves_and_copies() {
    let d = dump("fn take(s: String) {}\nfn main() {\n s := String::from(\"a\")\n n := 1\n m := n\n take(s)\n}");
    assert!(d.contains("call(move _"), "{d}");
    assert!(d.contains("= copy _"), "{d}");
}

#[test]
fn borrows_and_storage_dead() {
    let d = dump("fn main() {\n x := 1\n r := &x\n}");
    assert!(d.contains("= &_1"), "{d}");
    assert!(d.contains("dead _1"), "{d}");
}

#[test]
fn method_autoref_and_two_phase_order() {
    let d = dump("fn main() {\n v mut := Vec::new()\n v.push(v.len())\n}");
    let push_ref = d.find("&mut _1").expect(&d);
    let len_ref = d.find("= &_1").expect(&d);
    assert!(len_ref < push_ref, "len's borrow must come first:\n{d}");
}

#[test]
fn field_through_reference() {
    let d = dump("struct P {\n x: i32,\n}\nfn f(p: &P) -> i32 {\n return p.x\n}\nfn main() {}");
    assert!(d.contains("copy (*_1).x"), "{d}");
}

#[test]
fn match_ergonomics_bind_by_ref() {
    let d = dump("enum E {\n A(String),\n B,\n}\nfn f(e: &E) {\n match e {\n E::A(s) => println(s),\n E::B => {},\n }\n}\nfn main() {}");
    assert!(d.contains("&(*_1).A.0"), "{d}");
}

#[test]
fn closures_get_their_own_body_and_captures() {
    let b = lower("fn main() {\n x mut := 0\n f := || {\n x += 1\n }\n f()\n}");
    assert_eq!(b.len(), 2);
    assert_eq!(b[1].kind, BodyKind::Closure);
    assert!(b[0].to_string().contains("&mut _1"), "{}", b[0]);
}

#[test]
fn go_block_spawn() {
    let d = dump("fn main() {\n s := String::from(\"x\")\n go move {\n println(s)\n }\n}");
    assert!(
        d.contains("spawn GoBlock { is_move: true } (move _1) captures [s:Move]"),
        "{d}"
    );
}

#[test]
fn defer_runs_in_exit_block() {
    let b = lower("fn main() {\n defer println(\"bye\")\n x := 1\n}");
    let exit = &b[0].blocks[1];
    assert!(matches!(exit.term, Terminator::Return));
    assert!(exit
        .stmts
        .iter()
        .any(|s| matches!(s.kind, StmtKind::Eval(Rvalue::Pure(_)))));
}

#[test]
fn signature_flows() {
    let src = "fn first<'a>(a: &'a str, b: &str) -> &'a str {\n return a\n}\nfn one(a: &str) -> &str {\n return a\n}\nfn none(a: &str) -> i32 {\n return 1\n}\nfn main() {}";
    let b = lower(src);
    assert_eq!(b[0].return_flows, Some(vec![0]));
    assert_eq!(b[1].return_flows, Some(vec![0]));
    assert_eq!(b[2].return_flows, Some(vec![]));
}
