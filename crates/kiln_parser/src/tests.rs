//! Unit tests for the parser.

use crate::ast::*;
use crate::{parse, parse_expr};

/// Parses a file and asserts there were no errors.
fn ok(src: &str) -> File {
    let p = parse(src);
    assert!(
        p.errors.is_empty(),
        "unexpected errors:\n{}",
        p.errors
            .iter()
            .map(|e| e.render("test.kn", src))
            .collect::<String>()
    );
    p.file
}

/// Parses a file and returns the error messages.
fn errs(src: &str) -> Vec<String> {
    parse(src).errors.into_iter().map(|e| e.message).collect()
}

/// Parses the body of `fn main() { <body> }` and returns its statements.
fn stmts(body: &str) -> Vec<Stmt> {
    let file = ok(&format!("fn main() {{\n{body}\n}}"));
    match file.items.into_iter().next().unwrap().kind {
        ItemKind::Fn(f) => f.body.unwrap().stmts,
        _ => unreachable!(),
    }
}

fn stmt(body: &str) -> StmtKind {
    let s = stmts(body);
    assert_eq!(s.len(), 1, "expected one statement, got {s:#?}");
    s.into_iter().next().unwrap().kind
}

fn expr(src: &str) -> Expr {
    let (e, errors) = parse_expr(src);
    assert!(errors.is_empty(), "{errors:?}");
    e.unwrap()
}

/// Renders an expression as an S-expression, for compact precedence tests.
fn sx(e: &Expr) -> String {
    match &e.kind {
        ExprKind::Lit(Lit::Int { value, .. }) => value.to_string(),
        ExprKind::Lit(Lit::Bool(b)) => b.to_string(),
        ExprKind::Lit(Lit::Str(s)) => format!("{s:?}"),
        ExprKind::Path(p) => p.to_simple_string(),
        ExprKind::Binary(op, a, b) => format!("({op:?} {} {})", sx(a), sx(b)),
        ExprKind::Unary(op, a) => format!("({op:?} {})", sx(a)),
        ExprKind::Cast(a, _) => format!("(as {})", sx(a)),
        ExprKind::Call(f, args) => {
            let a: Vec<_> = args.iter().map(sx).collect();
            format!("(call {} {})", sx(f), a.join(" "))
        }
        ExprKind::MethodCall {
            receiver,
            method,
            args,
            ..
        } => {
            let a: Vec<_> = args.iter().map(sx).collect();
            format!("(.{} {} {})", method.name, sx(receiver), a.join(" "))
        }
        ExprKind::Field(a, f) => format!("(. {} {})", sx(a), f.name),
        ExprKind::TupleIndex(a, i) => format!("(. {} {i})", sx(a)),
        ExprKind::Index(a, i) => format!("([] {} {})", sx(a), sx(i)),
        ExprKind::Try(a) => format!("(? {})", sx(a)),
        ExprKind::Paren(a) => sx(a),
        ExprKind::Range {
            start,
            end,
            inclusive,
        } => format!(
            "({} {} {})",
            if *inclusive { "..=" } else { ".." },
            start.as_ref().map(|e| sx(e)).unwrap_or("_".into()),
            end.as_ref().map(|e| sx(e)).unwrap_or("_".into())
        ),
        other => format!(
            "<{}>",
            format!("{other:?}").split(['(', ' ', '{']).next().unwrap()
        ),
    }
}

fn sxe(src: &str) -> String {
    sx(&expr(src))
}

// ----- expressions ----------------------------------------------------------

#[test]
fn arithmetic_precedence() {
    assert_eq!(sxe("1 + 2 * 3"), "(Add 1 (Mul 2 3))");
    assert_eq!(sxe("(1 + 2) * 3"), "(Mul (Add 1 2) 3)");
    assert_eq!(sxe("1 - 2 - 3"), "(Sub (Sub 1 2) 3)");
    assert_eq!(sxe("a % b / c"), "(Div (Rem a b) c)");
}

#[test]
fn logical_and_comparison_precedence() {
    assert_eq!(sxe("a || b && c"), "(Or a (And b c))");
    assert_eq!(sxe("a == b && c < d"), "(And (Eq a b) (Lt c d))");
    assert_eq!(sxe("a + 1 >= b * 2"), "(Ge (Add a 1) (Mul b 2))");
}

#[test]
fn bitwise_precedence() {
    assert_eq!(sxe("a | b ^ c & d"), "(BitOr a (BitXor b (BitAnd c d)))");
    assert_eq!(sxe("1 << 2 + 3"), "(Shl 1 (Add 2 3))");
    assert_eq!(sxe("a & b == c"), "(Eq (BitAnd a b) c)");
}

#[test]
fn unary_operators() {
    assert_eq!(sxe("-a * b"), "(Mul (Neg a) b)");
    assert_eq!(sxe("!a && b"), "(And (Not a) b)");
    assert_eq!(sxe("*p + 1"), "(Add (Deref p) 1)");
    assert_eq!(sxe("&mut x"), "(RefMut x)");
    assert_eq!(sxe("&&x"), "(Ref (Ref x))");
    assert_eq!(sxe("-x.len()"), "(Neg (.len x ))");
}

#[test]
fn receive_operator() {
    assert_eq!(sxe("<-rx"), "(Recv rx)");
    assert_eq!(sxe("<-rx + 1"), "(Add (Recv rx) 1)");
}

#[test]
fn casts() {
    assert_eq!(sxe("a as i64 + 1"), "(Add (as a) 1)");
    assert_eq!(sxe("-x as u8"), "(as (Neg x))");
    assert_eq!(sxe("a * b as f64"), "(Mul a (as b))");
}

#[test]
fn postfix_chains() {
    assert_eq!(sxe("a.b.c"), "(. (. a b) c)");
    assert_eq!(sxe("f(1, 2)(3)"), "(call (call f 1 2) 3)");
    assert_eq!(sxe("x.foo(1).bar()?"), "(? (.bar (.foo x 1) ))");
    assert_eq!(sxe("a[i][j]"), "([] ([] a i) j)");
    assert_eq!(sxe("t.0"), "(. t 0)");
    assert_eq!(sxe("t.0.1"), "(. (. t 0) 1)");
    assert_eq!(sxe("5.secs()"), "(.secs 5 )");
}

#[test]
fn ranges() {
    assert_eq!(sxe("0..10"), "(.. 0 10)");
    assert_eq!(sxe("a..=b + 1"), "(..= a (Add b 1))");
    assert_eq!(sxe(".."), "(.. _ _)");
    assert_eq!(sxe("..n"), "(.. _ n)");
    assert_eq!(sxe("start.."), "(.. start _)");
}

#[test]
fn paths_and_turbofish() {
    assert_eq!(sxe("Shape::Circle"), "Shape::Circle");
    match expr("chan::<Job>(100)").kind {
        ExprKind::Call(f, args) => {
            assert_eq!(args.len(), 1);
            match f.kind {
                ExprKind::Path(p) => {
                    assert_eq!(p.segments[0].ident.name, "chan");
                    assert_eq!(p.segments[0].args.len(), 1);
                }
                k => panic!("{k:?}"),
            }
        }
        k => panic!("{k:?}"),
    }
    match expr("Vec::<Vec<i32>>::new()").kind {
        ExprKind::Call(f, _) => {
            assert!(matches!(f.kind, ExprKind::Path(ref p) if p.segments.len() == 2))
        }
        k => panic!("{k:?}"),
    }
    match expr("it.collect::<Vec<u8>>()").kind {
        ExprKind::MethodCall { generic_args, .. } => assert_eq!(generic_args.len(), 1),
        k => panic!("{k:?}"),
    }
}

#[test]
fn literals() {
    assert!(matches!(
        expr("42u8").kind,
        ExprKind::Lit(Lit::Int { value: 42, .. })
    ));
    assert!(matches!(expr("1.5").kind, ExprKind::Lit(Lit::Float { .. })));
    assert!(matches!(expr("'c'").kind, ExprKind::Lit(Lit::Char('c'))));
    assert!(matches!(expr("true").kind, ExprKind::Lit(Lit::Bool(true))));
    assert!(matches!(expr("r\"raw\"").kind, ExprKind::Lit(Lit::Str(ref s)) if s == "raw"));
}

#[test]
fn interpolated_strings() {
    match expr(r#""hi {name}, {a + b * 2}!""#).kind {
        ExprKind::Interp(parts) => {
            assert_eq!(parts.len(), 5);
            match &parts[3] {
                InterpPart::Expr(e) => assert_eq!(sx(e), "(Add a (Mul b 2))"),
                p => panic!("{p:?}"),
            }
        }
        k => panic!("{k:?}"),
    }
}

#[test]
fn interpolation_spans_point_into_file() {
    let src = r#""x = {foo}""#;
    match expr(src).kind {
        ExprKind::Interp(parts) => match &parts[1] {
            InterpPart::Expr(e) => assert_eq!(&src[e.span.start..e.span.end], "foo"),
            p => panic!("{p:?}"),
        },
        k => panic!("{k:?}"),
    }
}

#[test]
fn interpolation_syntax_error_is_reported() {
    let e = errs("fn main() {\n\tprintln(\"{a +}\")\n}");
    assert_eq!(e, vec!["expected expression, found end of file"]);
}

#[test]
fn tuples_arrays_and_parens() {
    assert!(matches!(expr("()").kind, ExprKind::Tuple(ref v) if v.is_empty()));
    assert!(matches!(expr("(1,)").kind, ExprKind::Tuple(ref v) if v.len() == 1));
    assert!(matches!(expr("(1, 2, 3)").kind, ExprKind::Tuple(ref v) if v.len() == 3));
    assert!(matches!(expr("(1)").kind, ExprKind::Paren(_)));
    assert!(matches!(expr("[1, 2]").kind, ExprKind::Array(ref v) if v.len() == 2));
    assert!(matches!(expr("[0; 16]").kind, ExprKind::ArrayRepeat(..)));
    assert!(matches!(expr("[]").kind, ExprKind::Array(ref v) if v.is_empty()));
}

#[test]
fn struct_literals() {
    match expr("Point { x: 1, y }").kind {
        ExprKind::StructLit { path, fields, base } => {
            assert_eq!(path.to_simple_string(), "Point");
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[1].name.name, "y");
            assert!(base.is_none());
        }
        k => panic!("{k:?}"),
    }
    match expr("Config { port: 80, ..defaults }").kind {
        ExprKind::StructLit { base, .. } => assert!(base.is_some()),
        k => panic!("{k:?}"),
    }
    match expr("Shape::Rect {\n\tw: 1,\n\th: 2,\n}").kind {
        ExprKind::StructLit { fields, .. } => assert_eq!(fields.len(), 2),
        k => panic!("{k:?}"),
    }
}

#[test]
fn closures() {
    match expr("|x| x + 1").kind {
        ExprKind::Closure {
            params,
            body,
            is_move,
            ..
        } => {
            assert_eq!(params.len(), 1);
            assert!(!is_move);
            assert_eq!(sx(&body), "(Add x 1)");
        }
        k => panic!("{k:?}"),
    }
    assert!(matches!(expr("|| 1").kind, ExprKind::Closure { ref params, .. } if params.is_empty()));
    assert!(matches!(
        expr("move |a: i32, b| { return a }").kind,
        ExprKind::Closure { is_move: true, .. }
    ));
    assert!(matches!(
        expr("|x| -> i32 { return x }").kind,
        ExprKind::Closure { ret: Some(_), .. }
    ));
}

#[test]
fn if_and_else_chains() {
    match expr("if a { 1 } else if b { 2 } else { 3 }").kind {
        ExprKind::If { els: Some(e), .. } => {
            assert!(matches!(e.kind, ExprKind::If { els: Some(_), .. }))
        }
        k => panic!("{k:?}"),
    }
    assert!(matches!(
        expr("if let Some(x) = opt { x }").kind,
        ExprKind::IfLet { .. }
    ));
}

#[test]
fn condition_does_not_take_struct_literal() {
    match expr("if x == Foo { 1 }").kind {
        ExprKind::If { cond, then, .. } => {
            assert_eq!(sx(&cond), "(Eq x Foo)");
            assert_eq!(then.stmts.len(), 1);
        }
        k => panic!("{k:?}"),
    }
    // ...but parentheses re-enable it.
    match expr("if x == (Foo { a: 1 }) { 1 }").kind {
        ExprKind::If { cond, .. } => assert!(matches!(cond.kind, ExprKind::Binary(..))),
        k => panic!("{k:?}"),
    }
}

#[test]
fn match_expression() {
    let src = "match s {\n\tShape::Circle(r) => 3 * r,\n\tShape::Rect { w, h } => w * h,\n\t1 | 2 => 0,\n\tn if n > 10 => n,\n\t_ => {\n\t\treturn 0\n\t},\n}";
    match expr(src).kind {
        ExprKind::Match(scrut, arms) => {
            assert_eq!(sx(&scrut), "s");
            assert_eq!(arms.len(), 5);
            assert!(matches!(arms[0].pat.kind, PatternKind::TupleStruct(..)));
            assert!(matches!(arms[1].pat.kind, PatternKind::Struct { .. }));
            assert!(matches!(arms[2].pat.kind, PatternKind::Or(ref v) if v.len() == 2));
            assert!(arms[3].guard.is_some());
            assert!(matches!(arms[4].pat.kind, PatternKind::Wild));
            assert!(matches!(arms[4].body.kind, ExprKind::Block(_)));
        }
        k => panic!("{k:?}"),
    }
}

#[test]
fn match_arms_without_commas_on_separate_lines() {
    match expr("match x {\n\t1 => a\n\t2 => b\n}").kind {
        ExprKind::Match(_, arms) => assert_eq!(arms.len(), 2),
        k => panic!("{k:?}"),
    }
}

#[test]
fn loops_and_labels() {
    assert!(matches!(
        expr("for i in 0..10 { }").kind,
        ExprKind::For { .. }
    ));
    assert!(matches!(
        expr("while x < 3 { }").kind,
        ExprKind::While { .. }
    ));
    assert!(matches!(
        expr("while let Some(v) = it.next() { }").kind,
        ExprKind::WhileLet { .. }
    ));
    match expr("'outer: loop { break 'outer }").kind {
        ExprKind::Loop {
            label: Some(l),
            body,
        } => {
            assert_eq!(l.name, "outer");
            assert!(matches!(
                &body.stmts[0].kind,
                StmtKind::Expr(Expr {
                    kind: ExprKind::Break(Some(_), None),
                    ..
                })
            ));
        }
        k => panic!("{k:?}"),
    }
    // `for x in xs {` must not parse `xs { }` as a struct literal.
    match expr("for p in points { use_it(p) }").kind {
        ExprKind::For { iter, body, .. } => {
            assert_eq!(sx(&iter), "points");
            assert_eq!(body.stmts.len(), 1);
        }
        k => panic!("{k:?}"),
    }
}

#[test]
fn select_expression() {
    let src = "select {\n\tr := <-res => use_it(r),\n\tv mut := <-other => {},\n\t<-timeout => {\n\t\tbreak\n\t},\n\ttx <- 42 => {},\n\t_ => {},\n}";
    match expr(src).kind {
        ExprKind::Select(arms) => {
            assert_eq!(arms.len(), 5);
            assert!(matches!(&arms[0].op, SelectOp::Recv { bind: Some(b), .. } if !b.mutable));
            assert!(matches!(&arms[1].op, SelectOp::Recv { bind: Some(b), .. } if b.mutable));
            assert!(matches!(&arms[2].op, SelectOp::Recv { bind: None, .. }));
            assert!(matches!(&arms[3].op, SelectOp::Send { .. }));
            assert!(matches!(&arms[4].op, SelectOp::Default));
        }
        k => panic!("{k:?}"),
    }
}

#[test]
fn bad_select_arm() {
    let e = errs("fn main() {\n\tselect {\n\t\tfoo() => {},\n\t}\n}");
    assert_eq!(
        e[0],
        "expected `<-` in select arm (`<-rx`, `v := <-rx`, `tx <- v`, or `_`), found `=>`"
    );
}

#[test]
fn return_break_continue() {
    assert!(matches!(expr("return").kind, ExprKind::Return(None)));
    assert!(matches!(
        expr("return x + 1").kind,
        ExprKind::Return(Some(_))
    ));
    assert!(matches!(
        expr("break 5").kind,
        ExprKind::Break(None, Some(_))
    ));
    assert!(matches!(expr("continue").kind, ExprKind::Continue(None)));
}

#[test]
fn reserved_keyword_rejected() {
    let (_, e) = parse_expr("x.await");
    assert_eq!(
        e[0].message,
        "`await` is reserved; Kiln uses `go` and channels for concurrency"
    );
    let (_, e) = parse_expr("yield");
    assert_eq!(e[0].message, "`yield` is a reserved keyword");
}

// ----- statements -----------------------------------------------------------

#[test]
fn let_statements() {
    match stmt("let x = 5") {
        StmtKind::Let {
            pat,
            ty: None,
            init: Some(_),
        } => {
            assert!(matches!(
                pat.kind,
                PatternKind::Ident { mutable: false, .. }
            ))
        }
        s => panic!("{s:?}"),
    }
    match stmt("let mut total: i64 = 0") {
        StmtKind::Let {
            pat, ty: Some(_), ..
        } => {
            assert!(matches!(pat.kind, PatternKind::Ident { mutable: true, .. }))
        }
        s => panic!("{s:?}"),
    }
    assert!(matches!(
        stmt("let (a, b) = pair"),
        StmtKind::Let {
            pat: Pattern {
                kind: PatternKind::Tuple(_),
                ..
            },
            ..
        }
    ));
    assert!(matches!(
        stmt("let x: u8"),
        StmtKind::Let { init: None, .. }
    ));
}

#[test]
fn short_declarations() {
    match stmt("x := 5") {
        StmtKind::ShortDecl {
            names,
            tuple: false,
            ..
        } => {
            assert_eq!(names[0].name.as_ref().unwrap().name, "x");
            assert!(!names[0].mutable);
        }
        s => panic!("{s:?}"),
    }
    match stmt("count mut := 0") {
        StmtKind::ShortDecl { names, .. } => assert!(names[0].mutable),
        s => panic!("{s:?}"),
    }
    match stmt("(a, b mut, _) := triple()") {
        StmtKind::ShortDecl {
            names, tuple: true, ..
        } => {
            assert_eq!(names.len(), 3);
            assert!(!names[0].mutable && names[1].mutable);
            assert!(names[2].name.is_none());
        }
        s => panic!("{s:?}"),
    }
    match stmt("v := <-rx") {
        StmtKind::ShortDecl { init, .. } => assert_eq!(sx(&init), "(Recv rx)"),
        s => panic!("{s:?}"),
    }
}

#[test]
fn short_decl_rejects_annotations_and_patterns() {
    assert_eq!(
        errs("fn main() {\n\tx: i32 := 5\n}"),
        vec!["type annotations require `let`"]
    );
    assert_eq!(
        errs("fn main() {\n\tSome(x) := opt\n}"),
        vec!["left side of `:=` must be a name, `name mut`, or a tuple of names"]
    );
}

#[test]
fn assignments() {
    match stmt("x = 1") {
        StmtKind::Assign { op: None, .. } => {}
        s => panic!("{s:?}"),
    }
    match stmt("self.total += a * 2") {
        StmtKind::Assign {
            op: Some(BinOp::Add),
            target,
            value,
        } => {
            assert_eq!(sx(&target), "(. self total)");
            assert_eq!(sx(&value), "(Mul a 2)");
        }
        s => panic!("{s:?}"),
    }
    assert!(matches!(
        stmt("a[i] <<= 1"),
        StmtKind::Assign {
            op: Some(BinOp::Shl),
            ..
        }
    ));
}

#[test]
fn channel_send() {
    match stmt("results <- process(job)") {
        StmtKind::Send { chan, value } => {
            assert_eq!(sx(&chan), "results");
            assert_eq!(sx(&value), "(call process job)");
        }
        s => panic!("{s:?}"),
    }
}

#[test]
fn go_statements() {
    assert!(matches!(
        stmt("go worker(1, rx)"),
        StmtKind::Go(GoTarget::Call(_))
    ));
    assert!(matches!(
        stmt("go srv.run()"),
        StmtKind::Go(GoTarget::Call(_))
    ));
    assert!(matches!(
        stmt("go move {\n\thandle(conn)\n}"),
        StmtKind::Go(GoTarget::Block { is_move: true, .. })
    ));
    assert!(matches!(
        stmt("go {\n}"),
        StmtKind::Go(GoTarget::Block { is_move: false, .. })
    ));
    assert_eq!(
        errs("fn main() {\n\tgo x\n}"),
        vec!["`go` requires a function call or a block (`go f(x)` or `go move { ... }`)"]
    );
}

#[test]
fn defer_statements() {
    assert!(matches!(stmt("defer file.close()"), StmtKind::Defer(_)));
    assert!(matches!(
        stmt("defer {\n\tcleanup()\n}"),
        StmtKind::Defer(Expr {
            kind: ExprKind::Block(_),
            ..
        })
    ));
}

#[test]
fn explicit_semicolons_and_multiple_statements() {
    assert_eq!(stmts("a := 1; b := 2\nc := 3").len(), 3);
}

#[test]
fn statements_after_block_expressions() {
    let s = stmts("if a {\n\tf()\n}\nfor x in xs {\n}\ng()");
    assert_eq!(s.len(), 3);
}

#[test]
fn nested_item_in_block() {
    assert!(matches!(stmt("fn helper() {}"), StmtKind::Item(_)));
}

#[test]
fn leading_dot_method_chain() {
    match stmt("x := items\n\t.iter()\n\t.count()") {
        StmtKind::ShortDecl { init, .. } => assert_eq!(sx(&init), "(.count (.iter items ) )"),
        s => panic!("{s:?}"),
    }
}

#[test]
fn binary_operator_continues_line() {
    match stmt("x := a +\n\tb") {
        StmtKind::ShortDecl { init, .. } => assert_eq!(sx(&init), "(Add a b)"),
        s => panic!("{s:?}"),
    }
}

#[test]
fn multiline_call_args_with_and_without_trailing_comma() {
    assert_eq!(stmts("f(\n\ta,\n\tb,\n)").len(), 1);
    assert_eq!(stmts("f(\n\ta,\n\tb\n)").len(), 1);
}

// ----- items ----------------------------------------------------------------

fn item(src: &str) -> Item {
    ok(src).items.into_iter().next().unwrap()
}

#[test]
fn function_items() {
    let it = item("pub fn add<T: Add + Copy>(a: T, b: T) -> T where T: Debug {\n\treturn a + b\n}");
    assert_eq!(it.vis, Visibility::Public);
    match it.kind {
        ItemKind::Fn(f) => {
            assert_eq!(f.name.name, "add");
            assert_eq!(f.generics.params.len(), 1);
            assert_eq!(f.generics.where_clause.len(), 1);
            assert_eq!(f.params.len(), 2);
            assert!(f.ret.is_some());
            assert_eq!(f.body.unwrap().stmts.len(), 1);
        }
        k => panic!("{k:?}"),
    }
}

#[test]
fn self_parameters() {
    let cases = [
        ("self", SelfParam::Value { mutable: false }),
        ("mut self", SelfParam::Value { mutable: true }),
        (
            "&self",
            SelfParam::Ref {
                lifetime: None,
                mutable: false,
            },
        ),
        (
            "&mut self",
            SelfParam::Ref {
                lifetime: None,
                mutable: true,
            },
        ),
    ];
    for (src, expected) in cases {
        match item(&format!("fn f({src}, x: i32) {{}}")).kind {
            ItemKind::Fn(f) => {
                assert_eq!(f.self_param, Some(expected), "{src}");
                assert_eq!(f.params.len(), 1);
            }
            k => panic!("{k:?}"),
        }
    }
    match item("fn f(&'a mut self) {}").kind {
        ItemKind::Fn(f) => assert!(matches!(
            f.self_param,
            Some(SelfParam::Ref { lifetime: Some(ref l), mutable: true }) if l.name == "a"
        )),
        k => panic!("{k:?}"),
    }
}

#[test]
fn unit_return_and_unsafe_fn() {
    match item("unsafe fn poke(p: *mut u8) {}").kind {
        ItemKind::Fn(f) => {
            assert!(f.is_unsafe);
            assert!(f.ret.is_none());
        }
        k => panic!("{k:?}"),
    }
}

#[test]
fn struct_items() {
    match item("pub struct Point {\n\tpub x: f64,\n\ty: f64,\n}").kind {
        ItemKind::Struct(s) => match s.fields {
            Fields::Named(f) => {
                assert_eq!(f.len(), 2);
                assert_eq!(f[0].vis, Visibility::Public);
                assert_eq!(f[1].vis, Visibility::Private);
            }
            f => panic!("{f:?}"),
        },
        k => panic!("{k:?}"),
    }
    // Newline-separated fields (no commas) are accepted too.
    match item("struct P {\n\tx: f64\n\ty: f64\n}").kind {
        ItemKind::Struct(s) => assert!(matches!(s.fields, Fields::Named(ref f) if f.len() == 2)),
        k => panic!("{k:?}"),
    }
    assert!(matches!(
        item("struct Meters(f64)").kind,
        ItemKind::Struct(StructDecl {
            fields: Fields::Tuple(_),
            ..
        })
    ));
    assert!(matches!(
        item("struct Marker").kind,
        ItemKind::Struct(StructDecl {
            fields: Fields::Unit,
            ..
        })
    ));
    match item("struct Parser<'src> {\n\tinput: &'src str,\n}").kind {
        ItemKind::Struct(s) => assert!(matches!(
            s.generics.params[0],
            GenericParam::Lifetime { .. }
        )),
        k => panic!("{k:?}"),
    }
}

#[test]
fn enum_items() {
    match item("enum Shape {\n\tCircle(f64),\n\tRect { w: f64, h: f64 },\n\tEmpty,\n\tCode = 7,\n}")
        .kind
    {
        ItemKind::Enum(e) => {
            assert_eq!(e.variants.len(), 4);
            assert!(matches!(e.variants[0].fields, Fields::Tuple(_)));
            assert!(matches!(e.variants[1].fields, Fields::Named(_)));
            assert!(matches!(e.variants[2].fields, Fields::Unit));
            assert!(e.variants[3].discriminant.is_some());
        }
        k => panic!("{k:?}"),
    }
}

#[test]
fn trait_items() {
    let src = "trait Describe: Debug + Clone {\n\ttype Out\n\tconst MAX: u32\n\tfn describe(&self) -> String\n\n\tfn shout(&self) -> String {\n\t\treturn self.describe()\n\t}\n}";
    match item(src).kind {
        ItemKind::Trait(t) => {
            assert_eq!(t.supertraits.len(), 2);
            assert_eq!(t.items.len(), 4);
            match &t.items[2].kind {
                ItemKind::Fn(f) => assert!(f.body.is_none()),
                k => panic!("{k:?}"),
            }
            match &t.items[3].kind {
                ItemKind::Fn(f) => assert!(f.body.is_some()),
                k => panic!("{k:?}"),
            }
        }
        k => panic!("{k:?}"),
    }
}

#[test]
fn impl_items() {
    match item("impl<T: Clone> From<io::Error> for Wrapper<T> where T: Debug {\n\tfn from(e: io::Error) -> Self {\n\t\treturn Wrapper::new(e)\n\t}\n}").kind {
        ItemKind::Impl(i) => {
            assert_eq!(i.trait_ref.unwrap().to_simple_string(), "From");
            assert!(matches!(i.self_ty.kind, TypeKind::Path(ref p) if p.segments[0].args.len() == 1));
            assert_eq!(i.generics.params.len(), 1);
            assert_eq!(i.generics.where_clause.len(), 1);
            assert_eq!(i.items.len(), 1);
        }
        k => panic!("{k:?}"),
    }
    match item("impl Point {\n}").kind {
        ItemKind::Impl(i) => assert!(i.trait_ref.is_none()),
        k => panic!("{k:?}"),
    }
}

#[test]
fn import_items() {
    match item("import std::net::{TcpListener, io::{self, BufReader as Reader}}").kind {
        ItemKind::Import(ImportTree::Group { prefix, items }) => {
            assert_eq!(prefix.len(), 2);
            assert_eq!(items.len(), 2);
            match &items[1] {
                ImportTree::Group { items, .. } => {
                    assert!(
                        matches!(&items[1], ImportTree::Path { alias: Some(a), .. } if a.name == "Reader")
                    )
                }
                t => panic!("{t:?}"),
            }
        }
        k => panic!("{k:?}"),
    }
    assert!(matches!(
        item("import std::fs").kind,
        ItemKind::Import(ImportTree::Path { .. })
    ));
    assert!(matches!(
        item("import std::prelude::*").kind,
        ItemKind::Import(ImportTree::Glob { .. })
    ));
    assert_eq!(
        errs("use std::fs"),
        vec!["`use` is reserved; Kiln imports with `import`"]
    );
}

#[test]
fn const_static_type_mod() {
    let f = ok("const MAX: usize = 10\nstatic mut COUNT: u64 = 0\ntype Map<K> = HashMap<K, String>\nmod util {\n\tfn f() {}\n}\nmod net");
    assert_eq!(f.items.len(), 5);
    assert!(matches!(
        f.items[1].kind,
        ItemKind::Static { mutable: true, .. }
    ));
    assert!(matches!(f.items[3].kind, ItemKind::Mod { items: Some(ref v), .. } if v.len() == 1));
    assert!(matches!(f.items[4].kind, ItemKind::Mod { items: None, .. }));
}

#[test]
fn visibility_modifiers() {
    assert_eq!(item("pub(pkg) fn f() {}").vis, Visibility::Package);
    assert_eq!(item("fn f() {}").vis, Visibility::Private);
}

#[test]
fn attributes_and_docs() {
    let it = item("/// A point.\n/// Second line.\n#[derive(Debug, Clone)]\n#[test]\nstruct P");
    assert_eq!(it.doc.as_deref(), Some("A point.\nSecond line."));
    assert_eq!(it.attrs.len(), 2);
    assert_eq!(it.attrs[0].name.name, "derive");
    assert_eq!(it.attrs[0].args.as_deref(), Some("Debug, Clone"));
    assert!(it.attrs[1].args.is_none());
}

#[test]
fn docs_attach_to_the_right_item() {
    let f = ok("/// first\nfn a() {\n\t/// stray\n\tx := 1\n}\n\nfn b() {}\n/// third\nfn c() {}");
    assert_eq!(f.items[0].doc.as_deref(), Some("first"));
    assert_eq!(f.items[1].doc, None);
    assert_eq!(f.items[2].doc.as_deref(), Some("third"));
}

// ----- types ----------------------------------------------------------------

fn ty(src: &str) -> TypeKind {
    match item(&format!("type X = {src}")).kind {
        ItemKind::TypeAlias { ty: Some(t), .. } => t.kind,
        k => panic!("{k:?}"),
    }
}

#[test]
fn types() {
    assert!(matches!(
        ty("&'a mut T"),
        TypeKind::Ref {
            lifetime: Some(_),
            mutable: true,
            ..
        }
    ));
    assert!(matches!(
        ty("*const u8"),
        TypeKind::Ptr { mutable: false, .. }
    ));
    assert!(matches!(ty("[u8]"), TypeKind::Slice(_)));
    assert!(matches!(ty("[u8; 32]"), TypeKind::Array(..)));
    assert!(matches!(ty("()"), TypeKind::Tuple(ref v) if v.is_empty()));
    assert!(matches!(ty("(i32, String)"), TypeKind::Tuple(ref v) if v.len() == 2));
    assert!(matches!(
        ty("fn(i32) -> bool"),
        TypeKind::Fn { ret: Some(_), .. }
    ));
    assert!(matches!(ty("dyn Describe + Send"), TypeKind::Dyn(ref b) if b.len() == 2));
    assert!(matches!(ty("impl Iterator<Item = u8>"), TypeKind::Impl(_)));
    assert!(matches!(ty("!"), TypeKind::Never));
    assert!(matches!(ty("_"), TypeKind::Infer));
    assert!(
        matches!(ty("&&str"), TypeKind::Ref { ref inner, .. } if matches!(inner.kind, TypeKind::Ref { .. }))
    );
}

#[test]
fn nested_generic_closers_are_split() {
    match ty("Vec<Vec<Option<u8>>>") {
        TypeKind::Path(p) => assert_eq!(p.segments[0].args.len(), 1),
        k => panic!("{k:?}"),
    }
    match ty("Result<(), io::Error>") {
        TypeKind::Path(p) => assert_eq!(p.segments[0].args.len(), 2),
        k => panic!("{k:?}"),
    }
}

#[test]
fn channel_types() {
    assert!(matches!(ty("chan<- Job"), TypeKind::ChanSend(_)));
    assert!(matches!(ty("<-chan Job"), TypeKind::ChanRecv(_)));
    assert!(
        matches!(ty("<-chan Vec<u8>"), TypeKind::ChanRecv(ref t) if matches!(t.kind, TypeKind::Path(_)))
    );
    assert_eq!(
        errs("type X = chan Job"),
        vec!["bidirectional `chan T` is not a type in Kiln"]
    );
}

// ----- patterns -------------------------------------------------------------

fn pat(src: &str) -> PatternKind {
    match stmt(&format!("let {src} = v")) {
        StmtKind::Let { pat, .. } => pat.kind,
        s => panic!("{s:?}"),
    }
}

#[test]
fn patterns() {
    assert!(matches!(pat("_"), PatternKind::Wild));
    assert!(matches!(pat("(a, _, ..)"), PatternKind::Tuple(ref v) if v.len() == 3));
    assert!(matches!(pat("[first, ..]"), PatternKind::Slice(_)));
    assert!(matches!(
        pat("&mut x"),
        PatternKind::Ref { mutable: true, .. }
    ));
    assert!(matches!(pat("Some(x)"), PatternKind::TupleStruct(..)));
    assert!(matches!(pat("Color::Red"), PatternKind::Path(_)));
    assert!(
        matches!(pat("Point { x, mut y, .. }"), PatternKind::Struct { rest: true, ref fields, .. } if fields.len() == 2)
    );
    assert!(matches!(
        pat("n @ 1..=9"),
        PatternKind::Ident { sub: Some(_), .. }
    ));
}

#[test]
fn literal_and_range_patterns() {
    match expr("match c {\n\t'a'..='z' => 1,\n\t-5 => 2,\n\t\"x\" => 3,\n\ttrue => 4,\n}").kind {
        ExprKind::Match(_, arms) => {
            assert!(matches!(
                arms[0].pat.kind,
                PatternKind::Range {
                    inclusive: true,
                    ..
                }
            ));
            assert!(matches!(
                arms[1].pat.kind,
                PatternKind::Lit(Lit::Int {
                    value: 5,
                    negative: true,
                    ..
                })
            ));
            assert!(matches!(arms[2].pat.kind, PatternKind::Lit(Lit::Str(_))));
            assert!(matches!(
                arms[3].pat.kind,
                PatternKind::Lit(Lit::Bool(true))
            ));
        }
        k => panic!("{k:?}"),
    }
}

// ----- errors and recovery ----------------------------------------------------

#[test]
fn recovers_between_items() {
    let p = parse("fn a( {}\nfn b() {}\nstruct C");
    assert!(!p.errors.is_empty());
    let names: Vec<_> = p
        .file
        .items
        .iter()
        .map(|i| match &i.kind {
            ItemKind::Fn(f) => f.name.name.clone(),
            ItemKind::Struct(s) => s.name.name.clone(),
            _ => String::new(),
        })
        .collect();
    assert_eq!(names, vec!["b", "C"]);
}

#[test]
fn recovers_between_statements() {
    let p = parse("fn main() {\n\tx := )\n\ty := 2\n\tz := ]\n\tw := 4\n}");
    assert_eq!(p.errors.len(), 2);
    match &p.file.items[0].kind {
        ItemKind::Fn(f) => assert_eq!(f.body.as_ref().unwrap().stmts.len(), 2),
        k => panic!("{k:?}"),
    }
}

#[test]
fn missing_statement_separator() {
    assert_eq!(
        errs("fn main() {\n\tx := 1 y := 2\n}"),
        vec!["expected newline or `;` after statement, found identifier `y`"]
    );
}

#[test]
fn error_spans_are_precise() {
    let src = "fn main() {\n\tx := 1 +\n}";
    let p = parse(src);
    assert_eq!(p.errors.len(), 1);
    assert_eq!(&src[p.errors[0].span.start..p.errors[0].span.end], "}");
}

#[test]
fn expected_item_message() {
    assert_eq!(
        errs("42"),
        vec!["expected item (`fn`, `struct`, `enum`, `trait`, `impl`, `import`, ...), found integer literal"]
    );
}

#[test]
fn lexer_errors_are_included() {
    assert_eq!(
        errs("fn main() {\n\tx := $\n}")[0],
        "unexpected character `$`"
    );
}

#[test]
fn spec_examples_parse() {
    for (name, src) in [
        ("hello", include_str!("../../../examples/hello.kn")),
        ("shapes", include_str!("../../../examples/shapes.kn")),
        ("workers", include_str!("../../../examples/workers.kn")),
        ("server", include_str!("../../../examples/server.kn")),
    ] {
        let p = parse(src);
        assert!(p.errors.is_empty(), "{name}: {:?}", p.errors);
        assert!(!p.file.items.is_empty());
    }
}
