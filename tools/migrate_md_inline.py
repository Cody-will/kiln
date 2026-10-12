"""Apply migrate_syntax.convert to inline `code` spans and untagged code
blocks that look like Kiln, in Markdown files."""
import re, sys
sys.path.insert(0, __file__.rsplit("/", 1)[0])
from migrate_syntax import convert, convert_md

TRIG = re.compile(r"\bfn\b|->|#\[|\bString\b|\bOption\b|\bResult\b|\(\)|\w: ")

def conv_span(m):
    s = m.group(1)
    if not TRIG.search(s) or "\n" in s:
        return m.group(0)
    try:
        t = convert(s)
    except Exception:
        return m.group(0)
    return "`" + t + "`"

def run(text):
    text = convert_md(text)
    parts = re.split(r"(```[^\n]*\n.*?```)", text, flags=re.S)
    for i, p in enumerate(parts):
        if p.startswith("```"):
            head, body = p.split("\n", 1)
            if head == "```" and re.search(r"^\s*(pub )?fn |#\[", body, re.M):
                parts[i] = head + "\n" + convert(body[:-3]) + "```"
        else:
            parts[i] = re.sub(r"`([^`\n]+)`", conv_span, p)
    return "".join(parts)

for f in sys.argv[1:]:
    s = open(f).read(); t = run(s)
    if t != s:
        open(f, "w").write(t); print("changed", f)
