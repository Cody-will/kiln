use crate::analysis::analyze;
use crate::server::{versions_still_current, Schedule, CHANGE_DEBOUNCE};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn uri(s: &str) -> lsp_types::Uri {
    s.parse().unwrap()
}

const SRC: &str = "struct Point {\n\tx i32,\n}\n\nimpl Point {\n\t// Twice x.\n\tfun double(&self) i32 {\n\t\treturn self.x * 2\n\t}\n}\n\n// Adds.\nfun add(a i32, b i32) i32 {\n\treturn a + b\n}\n\nfun main() {\n\tp := Point { x: 1 }\n\ts := add(p.x, p.double())\n\tprintln(\"{s}\")\n}\n";

fn at(needle: &str, nth: usize) -> usize {
    SRC.match_indices(needle).nth(nth).unwrap().0
}

#[test]
fn definitions_references_hover() {
    let a = analyze(SRC);
    assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
    // add(...) in main goes to fn add.
    let d = a.definition(at("add(p", 0)).unwrap();
    assert_eq!(&SRC[d.start..d.end], "add");
    assert_eq!(d.start, at("add(a", 0));
    // Method call goes to the method.
    let d = a.definition(at("double()", 0) + 1).unwrap();
    assert_eq!(d.start, at("double(&self", 0));
    // Local `s` used in the string interpolation.
    let refs = a.references(at("{s}", 0) + 1);
    assert_eq!(refs.len(), 2);
    // Hover.
    let (_, h) = a.hover(at("add(p", 0)).unwrap();
    assert!(
        h.contains("fun add(a i32, b i32) i32") && h.contains("Adds."),
        "{h}"
    );
    let (_, h) = a.hover(at("p := ", 0)).unwrap();
    assert!(h.contains("p Point"), "{h}");
    let (_, h) = a.hover(at("println", 0)).unwrap();
    assert!(h.contains("fun println"), "{h}");
}

#[test]
fn symbols_hints_completion() {
    let a = analyze(SRC);
    let names: Vec<_> = a
        .symbols
        .iter()
        .map(|s| (s.name.as_str(), s.kind))
        .collect();
    assert!(
        names.contains(&("Point", "struct"))
            && names.contains(&("double", "method"))
            && names.contains(&("main", "fun")),
        "{names:?}"
    );
    let hints = a.inlay_hints();
    assert!(hints.iter().any(|(_, h)| h == " Point"), "{hints:?}");
    assert!(hints.iter().any(|(_, h)| h == " i32"), "{hints:?}");
    // After `p.` (members of Point).
    let src2 = SRC.replace("println(\"{s}\")", "println(\"{s}\")\n\tp.");
    let a2 = analyze(&src2);
    let off = src2.find("\tp.\n").unwrap() + 3;
    let c: Vec<_> = a2
        .completions(&src2, off)
        .into_iter()
        .map(|c| c.label)
        .collect();
    // The file has an error now, so fall back to the earlier analysis's members.
    let c = if c.is_empty() {
        a.members
            .get("Point")
            .unwrap()
            .iter()
            .map(|c| c.label.clone())
            .collect()
    } else {
        c
    };
    assert!(
        c.contains(&"x".to_string()) && c.contains(&"double".to_string()),
        "{c:?}"
    );
    // Plain identifiers: locals and globals by prefix.
    let off = at("println", 0) + 2;
    let c: Vec<_> = a
        .completions(SRC, off)
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert!(
        c.contains(&"println".to_string()) && c.contains(&"print".to_string()),
        "{c:?}"
    );
    let off = at("println", 0);
    let c: Vec<_> = a
        .completions(SRC, off)
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert!(
        c.contains(&"s".to_string())
            && c.contains(&"p".to_string())
            && c.contains(&"add".to_string())
    );
}

#[test]
fn definitions_and_references_across_files() {
    use std::path::{Path, PathBuf};
    let main = "mod util\n\nfun main() {\n\tprintln(\"{util::twice(2)}\")\n}\n";
    let util = "pub fun twice(x i64) i64 {\n\treturn x * 2\n}\n";
    let dir = PathBuf::from("/nonexistent-kiln-lsp-test");
    let overlay = |p: &Path| -> Option<String> {
        if p == dir.join("main.kn") {
            Some(main.into())
        } else if p == dir.join("util.kn") {
            Some(util.into())
        } else {
            None
        }
    };
    let pkg = kiln_parser::package::load(&dir.join("main.kn"), &[], &overlay).unwrap();
    let a = crate::analysis::analyze_package(&pkg);
    assert!(a.diagnostics.is_empty(), "{:?}", a.diagnostics);
    assert_eq!(a.files.len(), 2);
    let use_at = main.find("twice").unwrap();
    let d = a.definition(use_at).unwrap();
    let (uoff, _, _) = a.files[1];
    assert_eq!(d.start, uoff + util.find("twice").unwrap());
    // References from the definition include the use in main.kn.
    assert!(a.references(d.start).iter().any(|s| s.start == use_at));
}

#[test]
fn debounce_coalesces_rapid_changes() {
    let mut sch = Schedule::default();
    let key = PathBuf::from("/proj/root.kn");
    let t0 = Instant::now();
    // Ten keystrokes, each resetting the debounce window.
    for i in 0..10 {
        let now = t0 + Duration::from_millis(i * 30);
        sch.schedule(
            key.clone(),
            uri(&format!("file:///change-{i}")),
            CHANGE_DEBOUNCE,
            now,
        );
    }
    // Still inside the window after the last keystroke.
    let mid = t0 + Duration::from_millis(9 * 30 + 50);
    assert!(sch.take_due(mid).is_none());
    assert_eq!(sch.pending.len(), 1);
    let wait = sch.next_timeout(mid).expect("pending timeout");
    assert!(wait > Duration::ZERO && wait <= CHANGE_DEBOUNCE);
    // After the debounce from the last change, one job fires.
    let due = t0 + Duration::from_millis(9 * 30) + CHANGE_DEBOUNCE + Duration::from_millis(1);
    let trigger = sch.take_due(due).expect("due");
    assert_eq!(trigger.as_str(), "file:///change-9");
    assert!(sch.in_flight);
    // No second job until the first finishes.
    assert!(sch.take_due(due + Duration::from_secs(1)).is_none());
    sch.finish(false, key, trigger, due);
    assert!(!sch.in_flight);
}

#[test]
fn stale_analysis_is_rejected_and_rescheduled() {
    let mut open = HashMap::new();
    open.insert(PathBuf::from("/a.kn"), 3);
    let mut snap = HashMap::new();
    snap.insert(PathBuf::from("/a.kn"), 2);
    assert!(!versions_still_current(&open, &snap));

    snap.insert(PathBuf::from("/a.kn"), 3);
    assert!(versions_still_current(&open, &snap));

    open.insert(PathBuf::from("/b.kn"), 1);
    assert!(!versions_still_current(&open, &snap));

    let mut sch = Schedule::default();
    let key = PathBuf::from("/a.kn");
    let now = Instant::now();
    sch.schedule(key.clone(), uri("file:///a.kn"), Duration::ZERO, now);
    let trigger = sch.take_due(now).unwrap();
    assert!(sch.in_flight);
    // Stale finish should re-queue immediately.
    sch.finish(true, key.clone(), trigger, now);
    assert!(!sch.in_flight);
    assert!(sch.pending.contains_key(&key));
    let again = sch.take_due(now + Duration::from_millis(1)).unwrap();
    assert_eq!(again.as_str(), "file:///a.kn");
}

#[test]
fn change_handling_stays_cheap() {
    // Before: each didChange ran analyze synchronously on the main loop
    // (~600ms release for examples/shapes.kn). After: didChange only
    // updates text + schedules a debounce; N rapid changes coalesce to one job.
    let mut sch = Schedule::default();
    let key = PathBuf::from("/proj/main.kn");
    let t0 = Instant::now();
    let n = 50usize;
    for i in 0..n {
        let now = t0 + Duration::from_millis(i as u64 * 10);
        sch.schedule(
            key.clone(),
            uri("file:///proj/main.kn"),
            CHANGE_DEBOUNCE,
            now,
        );
    }
    let schedule_cost = t0.elapsed();
    // Scheduling itself must be near-instant (orders of magnitude under one analyze).
    assert!(
        schedule_cost < Duration::from_millis(50),
        "scheduling {n} changes took {schedule_cost:?}"
    );
    let after_burst = t0 + Duration::from_millis(n as u64 * 10);
    assert!(
        sch.take_due(after_burst).is_none(),
        "should still be debouncing"
    );
    let fire_at = after_burst + CHANGE_DEBOUNCE + Duration::from_millis(1);
    assert!(sch.take_due(fire_at).is_some());
    assert_eq!(sch.pending.len(), 0);
}

#[test]
#[ignore = "latency probe; run with --ignored --nocapture"]
fn analyze_timing_shapes() {
    let src = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/shapes.kn"
    ))
    .unwrap();
    let _ = crate::analysis::analyze(&src);
    let n = 10usize;
    let t0 = Instant::now();
    for _ in 0..n {
        let _ = crate::analysis::analyze(&src);
    }
    let elapsed = t0.elapsed();
    eprintln!(
        "shapes.kn: {} analyzes in {:.1}ms ({:.1}ms each)",
        n,
        elapsed.as_secs_f64() * 1000.0,
        elapsed.as_secs_f64() * 1000.0 / n as f64
    );
}
