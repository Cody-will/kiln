//! Unit tests for the lexer.

use crate::token::Keyword;
use crate::{lex, lex_at, Span, StrPart, TokenKind};
use TokenKind::{Keyword as Kw, *};

fn kinds(src: &str) -> Vec<TokenKind> {
    let l = lex(src);
    assert!(l.errors.is_empty(), "unexpected errors: {:?}", l.errors);
    l.tokens.into_iter().map(|t| t.kind).collect()
}

fn errs(src: &str) -> Vec<String> {
    lex(src).errors.into_iter().map(|e| e.message).collect()
}

fn ident(s: &str) -> TokenKind {
    Ident(s.into())
}

fn kw(k: Keyword) -> TokenKind {
    Kw(k)
}

const NL: TokenKind = Semi { implicit: true };

fn int(v: u128) -> TokenKind {
    Int {
        value: v,
        suffix: None,
    }
}

#[test]
fn empty_input() {
    assert_eq!(kinds(""), vec![Eof]);
    assert_eq!(kinds("   \n\t\n"), vec![Eof]);
}

#[test]
fn identifiers_and_keywords() {
    assert_eq!(
        kinds("fn main foo_bar _x Self self"),
        vec![
            kw(Keyword::Fn),
            ident("main"),
            ident("foo_bar"),
            ident("_x"),
            kw(Keyword::SelfType),
            kw(Keyword::SelfValue),
            NL,
            Eof
        ]
    );
}

#[test]
fn every_keyword_roundtrips() {
    for (s, k) in Keyword::ALL {
        assert_eq!(kinds(s)[0], kw(*k), "keyword {s}");
        assert_eq!(k.as_str(), *s);
    }
}

#[test]
fn unicode_identifier() {
    assert_eq!(kinds("größe")[0], ident("größe"));
}

#[test]
fn underscore_is_its_own_token() {
    assert_eq!(kinds("_ =>"), vec![Underscore, FatArrow, Eof]);
}

#[test]
fn integer_literals() {
    assert_eq!(kinds("42")[0], int(42));
    assert_eq!(kinds("1_000_000")[0], int(1_000_000));
    assert_eq!(kinds("0xFF")[0], int(255));
    assert_eq!(kinds("0o17")[0], int(15));
    assert_eq!(kinds("0b1010")[0], int(10));
    assert_eq!(
        kinds("42u8")[0],
        Int {
            value: 42,
            suffix: Some("u8".into())
        }
    );
    assert_eq!(
        kinds("3i64")[0],
        Int {
            value: 3,
            suffix: Some("i64".into())
        }
    );
}

#[test]
fn float_literals() {
    match &kinds("2.75")[0] {
        Float {
            value,
            suffix: None,
            ..
        } => assert!((value - 2.75).abs() < 1e-12),
        t => panic!("{t:?}"),
    }
    match &kinds("1e9f32")[0] {
        Float {
            value,
            suffix: Some(s),
            ..
        } => {
            assert_eq!(*value, 1e9);
            assert_eq!(s, "f32");
        }
        t => panic!("{t:?}"),
    }
    match &kinds("2.5e-3")[0] {
        Float { value, .. } => assert_eq!(*value, 2.5e-3),
        t => panic!("{t:?}"),
    }
    assert!(matches!(kinds("7f64")[0], Float { .. }));
}

#[test]
fn ranges_and_method_on_int_are_not_floats() {
    assert_eq!(kinds("0..10"), vec![int(0), DotDot, int(10), NL, Eof]);
    assert_eq!(kinds("0..=10")[1], DotDotEq);
    assert_eq!(
        kinds("5.secs()"),
        vec![int(5), Dot, ident("secs"), LParen, RParen, NL, Eof]
    );
}

#[test]
fn bad_number_suffixes() {
    assert_eq!(
        errs("12abc"),
        vec!["invalid suffix `abc` for integer literal"]
    );
    assert_eq!(errs("1.5u8"), vec!["invalid suffix `u8` for float literal"]);
    assert_eq!(errs("0x"), vec!["missing digits after integer base prefix"]);
    assert_eq!(
        errs("999999999999999999999999999999999999999999"),
        vec!["integer literal is too large"]
    );
}

#[test]
fn plain_string() {
    assert_eq!(
        kinds(r#""hello""#)[0],
        Str(vec![StrPart::Text("hello".into())])
    );
    assert_eq!(kinds(r#""""#)[0], Str(vec![StrPart::Text(String::new())]));
}

#[test]
fn string_escapes() {
    assert_eq!(
        kinds(r#""a\n\t\\\"\x41\u{1F600}\0""#)[0],
        Str(vec![StrPart::Text("a\n\t\\\"A\u{1F600}\0".into())])
    );
}

#[test]
fn bad_escapes() {
    assert_eq!(errs(r#""\q""#), vec!["unknown escape `\\q`"]);
    assert_eq!(
        errs(r#""\xFF""#),
        vec!["invalid `\\x` escape (expected 00-7F)"]
    );
    assert_eq!(errs(r#""\u{110000}""#), vec!["invalid unicode escape"]);
}

#[test]
fn string_interpolation() {
    let src = r#""hi {name}, you are {age + 1}!""#;
    match &kinds(src)[0] {
        Str(parts) => {
            assert_eq!(parts.len(), 5);
            assert_eq!(parts[0], StrPart::Text("hi ".into()));
            assert_eq!(
                parts[1],
                StrPart::Interp {
                    src: "name".into(),
                    offset: 5
                }
            );
            assert_eq!(parts[2], StrPart::Text(", you are ".into()));
            assert!(matches!(&parts[3], StrPart::Interp { src, .. } if src == "age + 1"));
            assert_eq!(parts[4], StrPart::Text("!".into()));
        }
        t => panic!("{t:?}"),
    }
}

#[test]
fn interpolation_with_method_calls_and_nested_braces() {
    match &kinds(r#""len {body.len()} {f(Point { x: 1 })}""#)[0] {
        Str(parts) => {
            assert!(matches!(&parts[1], StrPart::Interp { src, .. } if src == "body.len()"));
            assert!(matches!(&parts[3], StrPart::Interp { src, .. } if src == "f(Point { x: 1 })"));
        }
        t => panic!("{t:?}"),
    }
}

#[test]
fn escaped_braces() {
    assert_eq!(
        kinds(r#""{{x}}""#)[0],
        Str(vec![StrPart::Text("{x}".into())])
    );
}

#[test]
fn interpolation_errors() {
    assert_eq!(
        errs(r#""a {} b""#),
        vec!["empty interpolation `{}` (use `{{}}` for literal braces)"]
    );
    assert_eq!(errs(r#""a } b""#), vec!["unmatched `}` in string literal"]);
    assert_eq!(
        errs("\"a {x\n\""),
        vec!["unterminated interpolation in string literal"]
    );
}

#[test]
fn unterminated_string() {
    assert_eq!(errs("\"abc"), vec!["unterminated string literal"]);
}

#[test]
fn raw_strings() {
    assert_eq!(kinds(r#"r"a\n{b}""#)[0], RawStr(r"a\n{b}".into()));
    assert_eq!(
        kinds(r###"r#"say "hi""#"###)[0],
        RawStr(r#"say "hi""#.into())
    );
    assert_eq!(errs(r#"r"abc"#), vec!["unterminated raw string literal"]);
}

#[test]
fn r_alone_is_identifier() {
    assert_eq!(kinds("r + rx")[0], ident("r"));
}

#[test]
fn char_literals() {
    assert_eq!(kinds("'a'")[0], Char('a'));
    assert_eq!(kinds(r"'\n'")[0], Char('\n'));
    assert_eq!(kinds(r"'\''")[0], Char('\''));
    assert_eq!(kinds("'é'")[0], Char('é'));
    assert_eq!(kinds(r"'\u{41}'")[0], Char('A'));
}

#[test]
fn char_errors() {
    assert_eq!(errs("''"), vec!["empty character literal"]);
    assert_eq!(
        errs("'ab'"),
        vec!["character literal may only contain one character"]
    );
}

#[test]
fn lifetimes_and_labels() {
    assert_eq!(
        kinds("&'a str"),
        vec![Amp, Lifetime("a".into()), ident("str"), NL, Eof]
    );
    assert_eq!(kinds("'outer: loop")[0], Lifetime("outer".into()));
    assert_eq!(kinds("'static")[0], Lifetime("static".into()));
}

#[test]
fn all_operators() {
    let src = "+ - * / % ^ ! & | && || << >> = == != < <= > >= += -= *= /= %= ^= &= |= <<= >>= \
               . .. ..= : :: := -> => <- ? # @ , ;";
    let expected = vec![
        Plus,
        Minus,
        Star,
        Slash,
        Percent,
        Caret,
        Bang,
        Amp,
        Pipe,
        AndAnd,
        OrOr,
        Shl,
        Shr,
        Eq,
        EqEq,
        Ne,
        Lt,
        Le,
        Gt,
        Ge,
        PlusEq,
        MinusEq,
        StarEq,
        SlashEq,
        PercentEq,
        CaretEq,
        AmpEq,
        PipeEq,
        ShlEq,
        ShrEq,
        Dot,
        DotDot,
        DotDotEq,
        Colon,
        ColonColon,
        ColonEq,
        Arrow,
        FatArrow,
        LArrow,
        Question,
        Hash,
        At,
        Comma,
        Semi { implicit: false },
        Eof,
    ];
    assert_eq!(kinds(src), expected);
}

#[test]
fn delimiters() {
    assert_eq!(
        kinds("( ) [ ] { }"),
        vec![LParen, RParen, LBracket, RBracket, LBrace, RBrace, NL, Eof]
    );
}

#[test]
fn short_decl_and_channel_ops() {
    assert_eq!(
        kinds("x mut := <-rx"),
        vec![
            ident("x"),
            kw(Keyword::Mut),
            ColonEq,
            LArrow,
            ident("rx"),
            NL,
            Eof
        ]
    );
    assert_eq!(
        kinds("tx <- v"),
        vec![ident("tx"), LArrow, ident("v"), NL, Eof]
    );
    assert_eq!(
        kinds("chan<- T"),
        vec![kw(Keyword::Chan), LArrow, ident("T"), NL, Eof]
    );
    assert_eq!(
        kinds("<-chan T"),
        vec![LArrow, kw(Keyword::Chan), ident("T"), NL, Eof]
    );
}

#[test]
fn comments_are_skipped() {
    assert_eq!(
        kinds("a // comment\nb"),
        vec![ident("a"), NL, ident("b"), NL, Eof]
    );
    assert_eq!(
        kinds("a /* x /* nested */ y */ b"),
        vec![ident("a"), ident("b"), NL, Eof]
    );
    assert_eq!(errs("/* open"), vec!["unterminated block comment"]);
}

#[test]
fn multiline_block_comment_acts_as_newline() {
    assert_eq!(
        kinds("a /* x\n y */ b"),
        vec![ident("a"), NL, ident("b"), NL, Eof]
    );
}

#[test]
fn doc_comments_collected() {
    let l = lex("/// Adds.\n/// Two lines.\n//// not doc\nfn f() {}");
    let docs: Vec<_> = l.doc_comments.iter().map(|(_, s)| s.as_str()).collect();
    assert_eq!(docs, vec!["Adds.", "Two lines."]);
}

#[test]
fn asi_after_statement_enders() {
    // ident, literal, `)`, `]`, `}`, `?`, return/break/continue/true/false
    for src in [
        "x", "1", "\"s\"", "f()", "a[0]", "{}", "x?", "return", "break", "continue", "true", "'c'",
    ] {
        let k = kinds(&format!("{src}\ny"));
        let semi_pos = k.iter().position(|t| *t == NL);
        assert!(semi_pos.is_some(), "no semicolon after {src}: {k:?}");
    }
}

#[test]
fn no_asi_after_operators_and_openers() {
    for src in ["a +", "f(", "x,", "{", "a.", "x :=", "if x &&", "tx <-"] {
        let k = kinds(&format!("{src}\ny"));
        assert_eq!(k.iter().filter(|t| **t == NL).count(), 1, "{src}: {k:?}"); // only after y
    }
}

#[test]
fn leading_dot_continues_line() {
    let k = kinds("x\n\t.foo()\n\t.bar()");
    assert_eq!(k.iter().filter(|t| **t == NL).count(), 1);
    // but `..` on next line does not continue
    let k = kinds("x\n..y");
    assert_eq!(k.iter().filter(|t| **t == NL).count(), 2);
}

#[test]
fn asi_at_eof_and_blank_lines() {
    assert_eq!(kinds("x\n\n\n"), vec![ident("x"), NL, Eof]);
}

#[test]
fn spans_are_byte_accurate() {
    let l = lex("let foo = 42u8");
    let spans: Vec<_> = l
        .tokens
        .iter()
        .map(|t| (t.span.start, t.span.end))
        .collect();
    assert_eq!(
        spans,
        vec![(0, 3), (4, 7), (8, 9), (10, 14), (14, 14), (14, 14)]
    );
}

#[test]
fn lex_at_offsets_spans() {
    let l = lex_at("x + y", 100);
    assert_eq!(l.tokens[0].span, Span::new(100, 101));
    assert_eq!(l.tokens[2].span, Span::new(104, 105));
}

#[test]
fn unexpected_character_recovers() {
    let l = lex("a $ b");
    assert_eq!(l.errors.len(), 1);
    assert_eq!(l.errors[0].message, "unexpected character `$`");
    assert_eq!(l.errors[0].span, Span::new(2, 3));
    assert_eq!(l.tokens.len(), 4); // a b ; eof
}

#[test]
fn lexes_full_function() {
    let src = "fn add(a: i32, b: i32) -> i32 {\n\treturn a + b\n}\n";
    let k = kinds(src);
    assert_eq!(
        k,
        vec![
            kw(Keyword::Fn),
            ident("add"),
            LParen,
            ident("a"),
            Colon,
            ident("i32"),
            Comma,
            ident("b"),
            Colon,
            ident("i32"),
            RParen,
            Arrow,
            ident("i32"),
            LBrace,
            kw(Keyword::Return),
            ident("a"),
            Plus,
            ident("b"),
            NL,
            RBrace,
            NL,
            Eof,
        ]
    );
}
