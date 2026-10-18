#!/usr/bin/env python3
"""Builds and runs the Kiln / Go / Rust benchmarks; writes results.json.

Usage: python3 bench/run.py [--runs N] [names...]

Needs: a release build of kiln with the LLVM backend
(cargo build --release -p kiln_cli --features llvm-dynamic; see docs/LLVM.md),
go, cargo, hyperfine, and oha (for the HTTP benchmark), all on PATH. Kiln
programs are built twice: `kiln-cranelift` (--release --backend=cranelift)
and `kiln-llvm` (--release --backend=llvm), plus `kilnc` (the self-hosted
compiler, `kilnc --build --release`).
"""
import json, os, subprocess, sys, time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
OUT = os.path.join(HERE, "out")
KILN = os.environ.get("KILN", os.path.join(ROOT, "target", "release", "kiln"))
BENCHES = ["fib", "nbody", "binarytrees", "spectralnorm", "fannkuch", "json", "hashmap", "pingpong", "workerpool"]
KILN_BACKENDS = ["cranelift", "llvm"]
KILNC = os.environ.get("KILNC", os.path.join(ROOT, "compiler", "target", "release", "kilnc"))
LANGS = ["kiln-" + b for b in KILN_BACKENDS] + ["kilnc", "go", "rust"]


def sh(cmd, **kw):
    subprocess.run(cmd, check=True, **kw)


def build():
    os.makedirs(OUT, exist_ok=True)
    sh(["cc", "-O2", "-o", os.path.join(OUT, "maxrss"), os.path.join(HERE, "maxrss.c")])
    sh(["cargo", "build", "--release", "-q"], cwd=os.path.join(HERE, "rust"))
    for b in BENCHES + ["http"]:
        for be in KILN_BACKENDS:
            sh([KILN, "build", "--release", f"--backend={be}", os.path.join(HERE, b, "main.kn"),
                "-o", os.path.join(OUT, f"{b}-kiln-{be}")])
        env = dict(os.environ, KILN_RT=os.environ.get("KILN_RT", os.path.join(ROOT, "target", "bootstrap", "rt", "libkiln_rt.a")),
                   KILNC_RESOLVE_DIR=os.path.join(ROOT, "crates", "kiln_resolve"))
        sh([KILNC, "--build", "--release", os.path.join(HERE, b, "main.kn"), "-o", os.path.join(OUT, f"{b}-kilnc")], env=env)
        sh(["go", "build", "-o", os.path.join(OUT, b + "-go"), os.path.join(HERE, b, "main.go")])
        os.replace(os.path.join(HERE, "rust", "target", "release", b), os.path.join(OUT, b + "-rust"))


def peak_rss_kib(exe):
    """Peak resident set size of one run, in KiB (from wait4's rusage)."""
    r = subprocess.run([os.path.join(OUT, "maxrss"), exe], capture_output=True, text=True, check=True)
    return int(r.stdout)


def output(exe):
    return subprocess.run([exe], capture_output=True, text=True, check=True).stdout


def run(names, runs):
    results = {}
    for b in names:
        outs = {l: output(os.path.join(OUT, f"{b}-{l}")) for l in LANGS}
        same = len(set(outs.values())) == 1
        if b == "json":
            # Go writes 99.0 as `99`, so only the decoded checksum must match.
            same = len({o.split()[-1] for o in outs.values()}) == 1
        cmds = [os.path.join(OUT, f"{b}-{l}") for l in LANGS]
        jf = os.path.join(OUT, f"{b}.json")
        sh(["hyperfine", "-N", "--warmup", "1", "--runs", str(runs), "--export-json", jf, *cmds],
           stdout=subprocess.DEVNULL)
        hf = json.load(open(jf))["results"]
        results[b] = {
            "same_output": same,
            **{l: {"median_s": r["median"], "mean_s": r["mean"], "stddev_s": r["stddev"],
                   "rss_kib": max(peak_rss_kib(c) for _ in range(3))}
               for l, r, c in zip(LANGS, hf, cmds)},
        }
        if not same:
            results[b]["outputs"] = outs
        print(b, {l: round(results[b][l]["median_s"], 3) for l in LANGS}, "same output" if same else "OUTPUTS DIFFER")
    return results


def http_bench(seconds=10, conns=64):
    res = {}
    for l in LANGS:
        srv = subprocess.Popen([os.path.join(OUT, f"http-{l}")])
        time.sleep(1)
        try:
            # Warm up, then measure.
            subprocess.run(["oha", "-z", "2s", "-c", str(conns), "--no-tui", "http://127.0.0.1:8080/hello"],
                           capture_output=True, check=True)
            r = subprocess.run(["oha", "-z", f"{seconds}s", "-c", str(conns), "--no-tui", "-j",
                                "http://127.0.0.1:8080/hello"], capture_output=True, text=True, check=True)
            j = json.loads(r.stdout)
            with open(f"/proc/{srv.pid}/status") as f:
                hwm = next(int(x.split()[1]) for x in f if x.startswith("VmHWM"))
            res[l] = {"req_per_s": j["summary"]["requestsPerSec"],
                      "p50_ms": j["latencyPercentiles"]["p50"] * 1000,
                      "p99_ms": j["latencyPercentiles"]["p99"] * 1000,
                      "success_rate": j["summary"]["successRate"],
                      "rss_kib": hwm}
        finally:
            srv.kill()
            srv.wait()
        print("http", l, round(res[l]["req_per_s"]), "req/s")
    return res


def main():
    args = sys.argv[1:]
    runs = 5
    if args[:1] == ["--runs"]:
        runs = int(args[1])
        args = args[2:]
    skip_build = "--no-build" in args
    args = [a for a in args if a != "--no-build"]
    names = args or BENCHES + ["http"]
    if not skip_build:
        build()
    path = os.path.join(HERE, "results.json")
    old = json.load(open(path)) if os.path.exists(path) else {}
    old.update(run([n for n in names if n != "http"], runs))
    json.dump(old, open(path, "w"), indent=2)
    if "http" in names:
        old["http"] = http_bench()
        json.dump(old, open(path, "w"), indent=2)


if __name__ == "__main__":
    main()
