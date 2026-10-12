#!/usr/bin/env python3
"""Applies migrate_syntax.convert to Kiln code inside Rust string literals
(test sources). Usage: migrate_rust_strings.py FILE.rs..."""
import re, sys, os
sys.path.insert(0, os.path.dirname(__file__))
from migrate_syntax import convert

KILN_HINT = re.compile(r"\bfn\b|#\[|->|\bString\b|\bOption\b|\bResult\b|\(\)|:\s*[&A-Za-z\[(]")


def unescape(s):
    out, i = [], 0
    while i < len(s):
        if s[i] == "\\" and i + 1 < len(s):
            c = s[i + 1]
            if c == "n": out.append("\n"); i += 2; continue
            if c == "t": out.append("\t"); i += 2; continue
            if c == "\\": out.append("\\"); i += 2; continue
            if c == '"': out.append('"'); i += 2; continue
            if c == "\n":
                # line continuation: skip newline and leading whitespace
                j = i + 2
                while j < len(s) and s[j] in " \t":
                    j += 1
                out.append("\x00CONT" + s[i + 2:j] + "\x00"); i = j; continue
            return None
        out.append(s[i]); i += 1
    return "".join(out)


def escape(s, orig):
    s = s.replace("\\", "\\\\").replace('"', '\\"')
    s = s.replace("\t", "\\t") if "\\t" in orig or "\t" not in orig else s
    s = s.replace("\n", "\\n") if "\\n" in orig or "\n" not in orig else s
    s = re.sub("\x00CONT(.*?)\x00", lambda m: "\\\n" + m.group(1), s, flags=re.S)
    return s


def process(src):
    out, i, n = [], 0, len(src)
    while i < n:
        if src.startswith("//", i):
            j = src.find("\n", i); j = n if j < 0 else j
            out.append(src[i:j]); i = j; continue
        m = re.match(r'(b?)r(#*)"', src[i:])
        if m and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")):
            h = m.group(2)
            start = i + len(m.group(0))
            end = src.find('"' + h, start)
            body = src[start:end]
            if KILN_HINT.search(body) and not m.group(1):
                body = convert(body)
            out.append(m.group(0) + body + '"' + h); i = end + 1 + len(h); continue
        if src[i] == "'":
            m = re.match(r"'(\\.|[^\\'])'|'\\u\{[0-9a-fA-F]+\}'", src[i:])
            if m:
                out.append(m.group(0)); i += len(m.group(0)); continue
        if src[i] == '"' and not (i > 0 and src[i - 1] == "b"):
            j = i + 1
            while src[j] != '"':
                j += 2 if src[j] == "\\" else 1
            body = src[i + 1:j]
            if KILN_HINT.search(body):
                u = unescape(body)
                if u is not None:
                    c = convert(u)
                    if c != u:
                        body = escape(c, body)
            out.append('"' + body + '"'); i = j + 1; continue
        out.append(src[i]); i += 1
    return "".join(out)


for f in sys.argv[1:]:
    s = open(f).read()
    t = process(s)
    if t != s:
        open(f, "w").write(t)
