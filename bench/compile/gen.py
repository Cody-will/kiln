#!/usr/bin/env python3
"""Generates the same large multi-file program in Kiln, Go and Rust.

usage: gen.py <out-dir> [modules] [fns-per-module]

Each module has a struct and `fns` functions (loops, a vector, a match,
string formatting); `main` calls every module's `run` and prints the sum,
which is the same in all three languages. The defaults give about 50,000
lines per language.
"""
import os
import sys

out = sys.argv[1]
MODS = int(sys.argv[2]) if len(sys.argv) > 2 else 50
FNS = int(sys.argv[3]) if len(sys.argv) > 3 else 45


def kn_fn(j):
    return f"""pub fun f{j}(n i64) i64 {{
	v mut := Vec::new()
	i mut := 0
	while i < n {{
		v.push(i * {j + 1} + {j % 7})
		i += 1
	}}
	s mut := 0
	for x in &v {{
		if *x % 3 == 0 {{
			s += *x
		}} else {{
			s -= 1
		}}
	}}
	p := Point {{ x: s, y: n }}
	t := match p.x % 4 {{
		0 => p.x + p.y,
		1 => p.x - p.y,
		_ => p.x * 2,
	}}
	label := "f{j}: {{t}}"
	return t + label.len() as i64
}}
"""


def go_fn(j):
    return f"""func m{{M}}f{j}(n int64) int64 {{
	v := make([]int64, 0)
	i := int64(0)
	for i < n {{
		v = append(v, i*{j + 1}+{j % 7})
		i += 1
	}}
	s := int64(0)
	for _, x := range v {{
		if x%3 == 0 {{
			s += x
		}} else {{
			s -= 1
		}}
	}}
	p := m{{M}}Point{{x: s, y: n}}
	var t int64
	switch p.x % 4 {{
	case 0:
		t = p.x + p.y
	case 1, -1:
		t = p.x - p.y
	default:
		t = p.x * 2
	}}
	label := fmt.Sprintf("f{j}: %d", t)
	return t + int64(len(label))
}}
"""


def rs_fn(j):
    return f"""pub fn f{j}(n: i64) -> i64 {{
    let mut v = Vec::new();
    let mut i = 0;
    while i < n {{
        v.push(i * {j + 1} + {j % 7});
        i += 1;
    }}
    let mut s = 0;
    for x in &v {{
        if *x % 3 == 0 {{
            s += *x;
        }} else {{
            s -= 1;
        }}
    }}
    let p = Point {{ x: s, y: n }};
    let t = match p.x % 4 {{
        0 => p.x + p.y,
        1 | -1 => p.x - p.y,
        _ => p.x * 2,
    }};
    let label = format!("f{j}: {{t}}");
    t + label.len() as i64
}}
"""


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


# Kiln's `%` truncates like Go and Rust, so `1 | -1` must match too.
def kn_fix(src):
    return src.replace("\t\t1 => p.x - p.y,", "\t\t1 | -1 => p.x - p.y,")


for m in range(MODS):
    kn = "struct Point {\n\tx i64,\n\ty i64,\n}\n\n"
    go = "package main\n\nimport \"fmt\"\n\ntype m{M}Point struct {\n\tx int64\n\ty int64\n}\n\n"
    rs = "struct Point {\n    x: i64,\n    y: i64,\n}\n\n"
    for j in range(FNS):
        kn += kn_fix(kn_fn(j)) + "\n"
        go += go_fn(j) + "\n"
        rs += rs_fn(j) + "\n"
    calls = " + ".join(f"f{j}(n)" for j in range(FNS))
    kn += f"pub fun run(n i64) i64 {{\n\treturn {calls}\n}}\n"
    go += f"func m{{M}}run(n int64) int64 {{\n\treturn {' + '.join(f'm{{M}}f{j}(n)' for j in range(FNS))}\n}}\n"
    rs += f"pub fn run(n: i64) -> i64 {{\n    {calls}\n}}\n"
    write(f"{out}/kn/src/m{m}.kn", kn)
    write(f"{out}/go/m{m}.go", go.replace("{M}", str(m)))
    write(f"{out}/rs/src/m{m}.rs", rs)

mods = "".join(f"mod m{m}\n" for m in range(MODS))
kn_sum = "\n".join(f"\ttotal += m{m}::run(10 + {m % 5})" for m in range(MODS))
write(f"{out}/kn/src/main.kn", f"{mods}\nfun main() {{\n\ttotal mut := 0\n{kn_sum}\n\tprintln(\"{{total}}\")\n}}\n")
write(f"{out}/kn/kiln.toml", "[package]\nname = \"big\"\nversion = \"0.1.0\"\n")
go_sum = "\n".join(f"\ttotal += m{m}run(10 + {m % 5})" for m in range(MODS))
write(f"{out}/go/main.go", f"package main\n\nimport \"fmt\"\n\nfunc main() {{\n\ttotal := int64(0)\n{go_sum}\n\tfmt.Println(total)\n}}\n")
write(f"{out}/go/go.mod", "module big\n\ngo 1.22\n")
rs_mods = "".join(f"mod m{m};\n" for m in range(MODS))
rs_sum = "\n".join(f"    total += m{m}::run(10 + {m % 5});" for m in range(MODS))
write(f"{out}/rs/src/main.rs", f"{rs_mods}\nfn main() {{\n    let mut total: i64 = 0;\n{rs_sum}\n    println!(\"{{total}}\");\n}}\n")
write(f"{out}/rs/Cargo.toml", "[package]\nname = \"big\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n")
