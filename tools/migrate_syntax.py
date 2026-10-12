#!/usr/bin/env python3
"""Rewrites Kiln source from the old syntax to the 2026-10 syntax revision.

    fn f(x: i32) -> Result<(), E>   =>   fun f(x i32) result<void, E>
    struct P { x: f64 }             =>   struct P { x f64 }
    let x: i32 = 5                  =>   let x i32 = 5
    #[derive(Debug)] / #[test]      =>   @derive(Debug) / @test
    String / Option / Result        =>   string / option / result
    Ok(())                          =>   Ok()

Usage: migrate_syntax.py FILE...   (.kn files, or .md files: ```kiln blocks)
"""
import re, sys

PUNCT3 = ["<<=", ">>=", "..=", "..."]
PUNCT2 = ["->", "=>", "::", ":=", "==", "!=", "<=", ">=", "&&", "||", "<-", "+=", "-=", "*=", "/=", "%=",
          "^=", "&=", "|=", "<<", ">>", ".."]
RENAME = {"String": "string", "Option": "option", "Result": "result", "fn": "fun"}


def lex(src):
    """Tokens as (kind, text). Kinds: ws, nl, comment, str, char, lifetime, ident, num, punct."""
    toks, i, n = [], 0, len(src)
    while i < n:
        c = src[i]
        if c == "\n":
            toks.append(("nl", c)); i += 1
        elif c in " \t\r":
            j = i
            while j < n and src[j] in " \t\r":
                j += 1
            toks.append(("ws", src[i:j])); i = j
        elif src.startswith("//", i):
            j = src.find("\n", i)
            j = n if j < 0 else j
            toks.append(("comment", src[i:j])); i = j
        elif src.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if src.startswith("/*", j):
                    depth += 1; j += 2
                elif src.startswith("*/", j):
                    depth -= 1; j += 2
                else:
                    j += 1
            toks.append(("comment", src[i:j])); i = j
        elif re.match(r'b?r#*"', src[i:i + 12]):
            m = re.match(r'(b?r)(#*)"', src[i:])
            hashes = m.group(2)
            end = src.find('"' + hashes, i + len(m.group(0)))
            j = end + 1 + len(hashes)
            toks.append(("str", src[i:j])); i = j
        elif c == '"' or (c == "b" and i + 1 < n and src[i + 1] == '"'):
            j = i + (2 if c == "b" else 1)
            out = src[i:j]
            while j < n and src[j] != '"':
                if src[j] == "\\":
                    out += src[j:j + 2]; j += 2
                elif src[j] == "{" and c != "b":
                    if src.startswith("{{", j):
                        out += "{{"; j += 2
                        continue
                    # interpolation: find the matching brace, convert its code
                    depth, k = 1, j + 1
                    while k < n and depth:
                        if src[k] == '"':
                            # nested string inside interpolation
                            k2 = k + 1
                            while k2 < n and src[k2] != '"':
                                k2 += 2 if src[k2] == "\\" else 1
                            k = k2 + 1
                            continue
                        if src[k] == "{":
                            depth += 1
                        elif src[k] == "}":
                            depth -= 1
                        k += 1
                    inner = src[j + 1:k - 1]
                    # split off a format spec `:...` at depth 0 (not `::`)
                    code, spec = split_spec(inner)
                    out += "{" + convert(code, expr_only=True) + spec + "}"
                    j = k
                else:
                    out += src[j]; j += 1
            out += '"'
            toks.append(("str", out)); i = j + 1
        elif c == "'" or (c == "b" and src.startswith("b'", i)):
            m = re.match(r"b?'(\\u\{[0-9a-fA-F]+\}|\\x[0-9a-fA-F]{2}|\\.|[^\\'])'", src[i:])
            if m:
                toks.append(("char", m.group(0))); i += len(m.group(0))
            else:
                m = re.match(r"'[A-Za-z_][A-Za-z0-9_]*", src[i:])
                toks.append(("lifetime", m.group(0))); i += len(m.group(0))
        elif c.isalpha() or c == "_":
            m = re.match(r"[A-Za-z_][A-Za-z0-9_]*", src[i:])
            toks.append(("ident", m.group(0))); i += len(m.group(0))
        elif c.isdigit():
            m = re.match(r"0[xob][0-9a-fA-F_]+|[0-9][0-9_]*(\.[0-9][0-9_]*)?([eE][+-]?[0-9]+)?[A-Za-z0-9_]*", src[i:])
            t = m.group(0)
            # `0..n`: don't eat the range dots
            toks.append(("num", t)); i += len(t)
        else:
            for p in PUNCT3 + PUNCT2:
                if src.startswith(p, i):
                    toks.append(("punct", p)); i += len(p)
                    break
            else:
                toks.append(("punct", c)); i += 1
    return toks


def split_spec(inner):
    depth = 0
    i = 0
    while i < len(inner):
        ch = inner[i]
        if ch in "([{":
            depth += 1
        elif ch in ")]}":
            depth -= 1
        elif ch == '"':
            i += 1
            while i < len(inner) and inner[i] != '"':
                i += 2 if inner[i] == "\\" else 1
        elif ch == ":" and depth == 0:
            if inner.startswith("::", i):
                i += 2
                continue
            return inner[:i], inner[i:]
        i += 1
    return inner, ""


SIG = ("ident", "num", "str", "char", "lifetime", "punct")


def convert(src, expr_only=False):
    toks = lex(src)
    out = [list(t) for t in toks]
    sig = [k for k, t in enumerate(toks) if t[0] in SIG]

    def text(k):
        return toks[k][1]

    def nxt(k, d=1):
        p = sig.index(k)
        q = p + d
        return sig[q] if 0 <= q < len(sig) else None

    def kill(k, eat_ws_after=True):
        out[k][1] = ""
        if eat_ws_after and k + 1 < len(out) and out[k + 1][0] == "ws":
            out[k + 1][1] = ""

    # Matching brackets.
    match = {}
    stack = []
    for k in sig:
        t = text(k)
        if t in "([{" and toks[k][0] == "punct":
            stack.append(k)
        elif t in ")]}" and toks[k][0] == "punct" and stack:
            o = stack.pop()
            match[o] = k
            match[k] = o

    # Frames where `name: Type` appears: fn params, struct/enum-variant field
    # lists, closure params. Record (open, close) ranges.
    colon_frames = []  # (open, close) token indexes; top-level `:` inside are annotations
    for idx, k in enumerate(sig):
        t = text(k)
        if toks[k][0] == "ident" and t == "fn":
            # fn NAME <...> ( ... )  -- params; or fn( for types (no names)
            j = idx + 1
            if j < len(sig) and toks[sig[j]][0] == "ident":
                j += 1
                angle = 0
                while j < len(sig):
                    tt = text(sig[j])
                    if tt == "<":
                        angle += 1
                    elif tt == ">":
                        angle -= 1
                    elif tt == ">>":
                        angle -= 2
                    elif tt == "(" and angle == 0:
                        colon_frames.append((sig[j], match.get(sig[j])))
                        break
                    elif tt in ("{", ";") and angle == 0:
                        break
                    j += 1
        elif toks[k][0] == "ident" and t in ("struct", "enum") and idx + 1 < len(sig):
            j = idx + 2
            angle = 0
            while j < len(sig):
                tt = text(sig[j])
                if tt == "<":
                    angle += 1
                elif tt == ">":
                    angle -= 1
                elif tt == ">>":
                    angle -= 2
                elif angle == 0 and tt in ("{", "(", ";"):
                    break
                j += 1
            if j < len(sig) and text(sig[j]) == "{":
                o = sig[j]
                c = match.get(o)
                if t == "struct":
                    colon_frames.append((o, c))
                else:
                    # enum: each `Variant {` inside the body at depth 1
                    depth = 0
                    for m in sig[sig.index(o) + 1:]:
                        if m == c:
                            break
                        tt = text(m)
                        if tt in "([{" and toks[m][0] == "punct":
                            if tt == "{" and depth == 0:
                                colon_frames.append((m, match.get(m)))
                            depth += 1
                        elif tt in ")]}" and toks[m][0] == "punct":
                            depth -= 1
        elif t == "|" and toks[k][0] == "punct":
            prev = sig[idx - 1] if idx > 0 else None
            pt = text(prev) if prev is not None else ""
            if pt in ("(", ",", "=", ":=", "{", "return", "move", "[", "=>", ";") or prev is None:
                # closure params until the next `|` at the same depth
                depth = 0
                for m in sig[idx + 1:]:
                    tt = text(m)
                    if tt in "([{" and toks[m][0] == "punct":
                        depth += 1
                    elif tt in ")]}" and toks[m][0] == "punct":
                        depth -= 1
                    elif tt == "|" and depth == 0:
                        colon_frames.append((k, m))
                        break

    def top_level_colons(o, c):
        res, depth = [], 0
        for m in sig[sig.index(o) + 1:]:
            if m == c:
                break
            tt = text(m)
            if toks[m][0] == "punct" and tt in "([{":
                depth += 1
            elif toks[m][0] == "punct" and tt in ")]}":
                depth -= 1
            elif tt == ":" and depth == 0:
                res.append(m)
        return res

    colons = set()
    for o, c in colon_frames:
        if c is not None:
            colons.update(top_level_colons(o, c))
    # let / const / static annotations: first `:` at the same depth before `=`, newline, `;`
    for idx, k in enumerate(sig):
        if toks[k][0] == "ident" and text(k) in ("let", "const", "static"):
            if text(k) == "const" and idx + 1 < len(sig) and text(sig[idx + 1]) in ("fn", "{"):
                continue
            depth = 0
            m = k + 1
            while m < len(toks):
                kind, tt = toks[m]
                if kind == "nl" and depth == 0:
                    break
                if kind == "punct":
                    if tt in "([{":
                        depth += 1
                    elif tt in ")]}":
                        depth -= 1
                        if depth < 0:
                            break
                    elif depth == 0 and tt in ("=", ";", ":="):
                        break
                    elif depth == 0 and tt == ":":
                        colons.add(m)
                        break
                m += 1
    # Const generics `<const N: usize>`
    for idx, k in enumerate(sig):
        if text(k) == "const" and idx > 0 and text(sig[idx - 1]) in ("<", ",") and idx + 2 < len(sig) and text(sig[idx + 2]) == ":":
            colons.add(sig[idx + 2])

    for m in colons:
        out[m][1] = ""
        # `x: T` -> `x T` (one space)
        if m + 1 < len(out) and out[m + 1][0] == "ws":
            out[m + 1][1] = " "
        else:
            out[m][1] = " "

    # `-> ()` disappears; `-> T` becomes ` T`; `()` in generic args -> void
    for idx, k in enumerate(sig):
        t = text(k)
        if t == "->":
            a = sig[idx + 1] if idx + 1 < len(sig) else None
            b = sig[idx + 2] if idx + 2 < len(sig) else None
            if a is not None and text(a) == "(" and b is not None and text(b) == ")":
                out[k][1] = ""
                out[a][1] = ""
                out[b][1] = ""
                # whitespace before `->`
                if k > 0 and out[k - 1][0] == "ws":
                    out[k - 1][1] = ""
                if b + 1 < len(out) and out[b + 1][0] == "ws" and not (b + 2 < len(out) and toks[b + 2][1] == "{"):
                    out[b + 1][1] = ""
            else:
                kill(k)
        elif t == "(" and idx + 1 < len(sig) and text(sig[idx + 1]) == ")":
            prev = sig[idx - 1] if idx > 0 else None
            pt = text(prev) if prev is not None else ""
            if pt == "<" or (pt == "," and in_angle(toks, sig, idx)):
                out[k][1] = "void"
                out[sig[idx + 1]][1] = ""
            elif prev in colons:
                out[k][1] = "void"
                out[sig[idx + 1]][1] = ""
            elif pt == "(" and idx >= 2 and text(sig[idx - 2]) == "Ok" and idx + 2 < len(sig) and text(sig[idx + 2]) == ")":
                out[k][1] = ""
                out[sig[idx + 1]][1] = ""
        elif t == "#" and idx + 1 < len(sig) and text(sig[idx + 1]) == "[":
            o = sig[idx + 1]
            c = match.get(o)
            out[k][1] = "@"
            out[o][1] = ""
            if c is not None:
                out[c][1] = ""
        elif toks[k][0] == "ident" and t in RENAME:
            prev = sig[idx - 1] if idx > 0 else None
            if prev is not None and text(prev) == "." and t != "fn":
                continue
            out[k][1] = RENAME[t]
    return "".join(t[1] for t in out)


def in_angle(toks, sig, idx):
    """Whether sig[idx] sits directly inside `<...>` (a generic argument list)."""
    depth_angle = 0
    j = idx - 1
    while j >= 0:
        t = toks[sig[j]][1]
        if t == ">":
            depth_angle += 1
        elif t == ">>":
            depth_angle += 2
        elif t == "<":
            if depth_angle == 0:
                return True
            depth_angle -= 1
        elif t in ("(", "{", "[", ";", "=") and depth_angle == 0:
            return False
        elif t in (")", "]", "}"):
            # skip balanced group backwards
            close, open_ = t, {")": "(", "]": "[", "}": "{"}[t]
            d = 1
            j -= 1
            while j >= 0 and d:
                tt = toks[sig[j]][1]
                if tt == close:
                    d += 1
                elif tt == open_:
                    d -= 1
                j -= 1
            continue
        j -= 1
    return False


def convert_md(text):
    parts = re.split(r"(```kiln\n.*?```)", text, flags=re.S)
    for i, p in enumerate(parts):
        if p.startswith("```kiln\n"):
            parts[i] = "```kiln\n" + convert(p[len("```kiln\n"):-3]) + "```"
    return "".join(parts)


if __name__ == "__main__":
    for f in sys.argv[1:]:
        s = open(f).read()
        t = convert_md(s) if f.endswith(".md") else convert(s)
        if t != s:
            open(f, "w").write(t)
