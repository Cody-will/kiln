#!/usr/bin/env python3
"""Compile-time benchmark: Kiln (Rust `kiln` and the self-hosted `kilnc`)
against Go and Rust on equivalent programs. Writes results.json and
prints a Markdown table (see COMPILE.md).

usage: python3 bench/compile/run.py [--runs N] [--only hello,mid,big] [--compilers SUBSTRING]

Programs:
  hello  hello world
  mid    the JSON benchmark (bench/json): derive/serde, std JSON
  big    a generated multi-file program, ~50k lines per language (gen.py)
Builds:
  clean        nothing cached (Go keeps its standard-library cache; Cargo
               keeps built dependencies)
  incremental  after a one-line change to one function
Needs: target/release/kiln (with LLVM), compiler/target/release/kilnc,
clang, go, cargo.
"""
import json, os, re, shutil, statistics, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
WORK = os.environ.get("WORK", "/tmp/kiln-compile-bench")
KILN = os.environ.get("KILN", f"{ROOT}/target/release/kiln")
KILNC = os.environ.get("KILNC", f"{ROOT}/compiler/target/release/kilnc")
RT = f"{WORK}/rt/libkiln_rt.a"
ENV = dict(os.environ, KILN_RT=RT, KILNC_RESOLVE_DIR=f"{ROOT}/crates/kiln_resolve", KILN_RUN_FLAGS="")

HELLO = {
    "kn": 'fun main() {\n\tprintln("hello, world")\n}\n',
    "go": 'package main\n\nimport "fmt"\n\nfunc main() {\n\tfmt.Println("hello, world")\n}\n',
    "rs": 'fn main() {\n    println!("hello, world");\n}\n',
}
# One-line edits: (file relative to the project, old, new).
EDITS = {
    "hello": {"kn": ("main.kn", "hello, world", "hello, there"),
              "go": ("main.go", "hello, world", "hello, there"),
              "rs": ("src/main.rs", "hello, world", "hello, there")},
    "mid": {"kn": ("main.kn", "0..50000", "0..50001"),
            "go": ("main.go", "50000", "50001"),
            "rs": ("src/main.rs", "0..50000", "0..50001")},
    "big": {"kn": ("src/m7.kn", "v.push(i * 1 + 0)", "v.push(i * 1 + 1)"),
            "go": ("m7.go", "v = append(v, i*1+0)", "v = append(v, i*1+1)"),
            "rs": ("src/m7.rs", "v.push(i * 1 + 0)", "v.push(i * 1 + 1)")},
}


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


def setup():
    shutil.rmtree(WORK, ignore_errors=True)
    subprocess.run([KILN, "runtime", "--write", f"{WORK}/rt"], check=True)
    cargo_toml = '[package]\nname = "p"\nversion = "0.1.0"\nedition = "2021"\n\n[workspace]\n'
    for name in ["hello", "mid"]:
        d = f"{WORK}/{name}"
        if name == "hello":
            src = HELLO
        else:
            src = {e: open(f"{ROOT}/bench/json/main.{e}").read() for e in ["kn", "go", "rs"]}
        write(f"{d}/kn/main.kn", src["kn"])
        write(f"{d}/go/main.go", src["go"])
        write(f"{d}/go/go.mod", "module p\n\ngo 1.22\n")
        deps = '\n[dependencies]\nserde = { version = "1", features = ["derive"] }\nserde_json = "1"\n' if name == "mid" else ""
        write(f"{d}/rs/Cargo.toml", cargo_toml + deps)
        write(f"{d}/rs/src/main.rs", src["rs"])
    subprocess.run([sys.executable, f"{HERE}/gen.py", f"{WORK}/big", "50", "40"], check=True)
    # A Go build cache holding only the standard library: copied fresh for
    # each clean Go build.
    # (built from a separate program, so none of the measured packages are in it).
    env = dict(os.environ, GOCACHE=f"{WORK}/gocache-std")
    write(f"{WORK}/warm/main.go", 'package main\n\nimport (\n\t"encoding/json"\n\t"fmt"\n)\n\n'
          'func main() {\n\tb, _ := json.Marshal([]int{1})\n\tfmt.Println(string(b))\n}\n')
    write(f"{WORK}/warm/go.mod", "module warm\n\ngo 1.22\n")
    subprocess.run(["go", "build", "-o", f"{WORK}/x", "."], cwd=f"{WORK}/warm", env=env, check=True)
    # Cargo dependencies built once, in both profiles.
    for prof in [[], ["--release"]]:
        subprocess.run(["cargo", "build", "-q"] + prof, cwd=f"{WORK}/mid/rs", check=True)


# (label, language, clean step, build command) per compiler.
def compilers(prog):
    d = f"{WORK}/{prog}"
    kn = f"{d}/kn"
    single = prog != "big"
    src = ["main.kn"] if single else []
    out = f"{WORK}/out-{prog}"
    kc_src = "main.kn" if single else "src/main.kn"

    def rm(*paths):
        def f():
            for p in paths:
                for q in [p] + [os.path.join(os.path.dirname(p), x) for x in os.listdir(os.path.dirname(p))
                                if x.startswith(os.path.basename(p) + ".")]:
                    if os.path.isdir(q):
                        shutil.rmtree(q)
                    elif os.path.exists(q):
                        os.remove(q)
        return f

    def go_clean():
        # `go build -o` skips linking when the output is already up to date.
        if os.path.exists(f"{out}-go"):
            os.remove(f"{out}-go")
        shutil.rmtree(f"{WORK}/gocache", ignore_errors=True)
        shutil.copytree(f"{WORK}/gocache-std", f"{WORK}/gocache")

    def cargo_clean(rel):
        def f():
            subprocess.run(["cargo", "clean", "-q", "-p", "p" if single else "big"] + rel, cwd=f"{d}/rs")
        return f

    kiln_out = ["-o", f"{out}-cl"] if single else []
    kiln_out_r = ["-o", f"{out}-ll"] if single else []
    return [
        ("kiln (Cranelift, debug)", "kn", rm(f"{out}-cl", f"{kn}/target"), [KILN, "build"] + src + kiln_out, kn),
        ("kiln (LLVM, release)", "kn", rm(f"{out}-ll", f"{kn}/target"), [KILN, "build", "--release", "--backend=llvm"] + src + kiln_out_r, kn),
        ("kilnc (debug)", "kn", rm(f"{out}-kc"), [KILNC, "--build", kc_src, "-o", f"{out}-kc"], kn),
        ("kilnc (release)", "kn", rm(f"{out}-kcr"), [KILNC, "--build", "--release", kc_src, "-o", f"{out}-kcr"], kn),
        ("go build", "go", go_clean, ["go", "build", "-o", f"{out}-go", "."], f"{d}/go"),
        ("cargo build (debug)", "rs", cargo_clean([]), ["cargo", "build", "-q"], f"{d}/rs"),
        ("cargo build (release)", "rs", cargo_clean(["--release"]), ["cargo", "build", "-q", "--release"], f"{d}/rs"),
    ]


def timed(cmd, cwd):
    env = dict(ENV, GOCACHE=f"{WORK}/gocache")
    t = time.perf_counter()
    r = subprocess.run(cmd, cwd=cwd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    dt = time.perf_counter() - t
    if r.returncode != 0:
        raise SystemExit(f"{cmd} failed in {cwd}:\n{r.stderr.decode()}")
    return dt


def edit(prog, lang, cwd, forward):
    rel, old, new = EDITS[prog][lang]
    p = os.path.join(cwd, rel)
    text = open(p).read()
    a, b = (old, new) if forward else (new, old)
    assert a in text, (p, a)
    write(p, text.replace(a, b, 1))


def main():
    runs = 3
    only = ["hello", "mid", "big"]
    args = sys.argv[1:]
    if "--runs" in args:
        runs = int(args[args.index("--runs") + 1])
    if "--only" in args:
        only = args[args.index("--only") + 1].split(",")
    pick = None
    if "--compilers" in args:
        pick = args[args.index("--compilers") + 1]
    old = {}
    if pick and os.path.exists(f"{HERE}/results.json"):
        old = json.load(open(f"{HERE}/results.json"))
    setup()
    results = old
    for prog in only:
        for label, lang, clean, cmd, cwd in compilers(prog):
            if pick and pick not in label:
                continue
            cl, inc = [], []
            for _ in range(runs):
                clean()
                cl.append(timed(cmd, cwd))
                edit(prog, lang, cwd, True)
                inc.append(timed(cmd, cwd))
                edit(prog, lang, cwd, False)
                timed(cmd, cwd)  # back to the original (not measured)
            results.setdefault(prog, {})[label] = {
                "clean": statistics.median(cl), "incremental": statistics.median(inc)}
            print(f"{prog:6} {label:24} clean {statistics.median(cl):7.2f}s  incremental {statistics.median(inc):7.2f}s", flush=True)
    with open(f"{HERE}/results.json", "w") as f:
        json.dump(results, f, indent=1)
    labels = list(next(iter(results.values())).keys())
    print("\n| Compiler | " + " | ".join(f"{p} clean | {p} incr." for p in results) + " |")
    print("|---|" + "---:|" * (2 * len(results)))
    for l in labels:
        cells = []
        for p in results:
            r = results[p][l]
            cells += [f"{r['clean']:.2f} s", f"{r['incremental']:.2f} s"]
        print(f"| {l} | " + " | ".join(cells) + " |")


if __name__ == "__main__":
    main()
