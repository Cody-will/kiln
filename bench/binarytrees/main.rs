struct Node {
    left: Option<Box<Node>>,
    right: Option<Box<Node>>,
}

fn make(d: i64) -> Box<Node> {
    if d == 0 {
        return Box::new(Node { left: None, right: None });
    }
    Box::new(Node { left: Some(make(d - 1)), right: Some(make(d - 1)) })
}

fn check(n: &Node) -> i64 {
    match (&n.left, &n.right) {
        (Some(l), Some(r)) => 1 + check(l) + check(r),
        _ => 1,
    }
}

fn main() {
    let max = 18;
    let stretch = make(max + 1);
    println!("stretch tree of depth {}\t check: {}", max + 1, check(&stretch));
    drop(stretch);
    let long = make(max);
    let mut d = 4;
    while d <= max {
        let iters = 1 << (max - d + 4);
        let mut c = 0;
        for _ in 0..iters {
            c += check(&make(d));
        }
        println!("{}\t trees of depth {}\t check: {}", iters, d, c);
        d += 2;
    }
    println!("long lived tree of depth {}\t check: {}", max, check(&long));
}
