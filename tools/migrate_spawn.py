"""Rewrite the task keyword `go` to `spawn` in .kn, Rust test sources, and
Markdown (code blocks and inline code only)."""
import re, sys
KW = re.compile(r"(?<![.\w:'\"])go(?= +(?:move\b|[A-Za-z_{(]))")
TICK = re.compile(r"`[^`\n]*`")

def code_line(line):
    i = line.find("//")
    if i >= 0:
        return KW.sub("spawn", line[:i]) + TICK.sub(lambda m: KW.sub("spawn", re.sub(r"`go`", "`spawn`", m.group(0))), line[i:])
    return KW.sub("spawn", line)

def md(text):
    out, inblock = [], False
    for line in text.split("\n"):
        if line.startswith("```"):
            inblock = not inblock; out.append(line); continue
        if inblock:
            out.append(code_line(line))
        else:
            out.append(TICK.sub(lambda m: KW.sub("spawn", m.group(0).replace("`go`", "`spawn`")), line))
    return "\n".join(out)

for f in sys.argv[1:]:
    s = open(f).read()
    t = md(s) if f.endswith(".md") else "\n".join(code_line(l) for l in s.split("\n"))
    if t != s:
        open(f, "w").write(t); print(f)
