use std::collections::HashMap;

fn main() {
    let mut m = HashMap::new();
    let n: i64 = 1_000_000;
    for i in 0..n {
        m.insert(i * 7, i);
    }
    let mut hits = 0i64;
    for i in 0..2 * n {
        if let Some(v) = m.get(&i) {
            hits += *v;
        }
    }
    for i in 0..n / 2 {
        m.remove(&(i * 14));
    }
    let mut s = HashMap::new();
    for i in 0..200_000i64 {
        s.insert(format!("key-{i}"), i);
    }
    let mut shits = 0;
    for i in 0..200_000i64 {
        if s.contains_key(&format!("key-{}", i * 3)) {
            shits += 1;
        }
    }
    println!("{} {} {}", hits, m.len(), shits);
}
