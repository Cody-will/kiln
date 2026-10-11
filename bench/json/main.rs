use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone)]
struct User {
    id: i64,
    name: String,
    email: String,
    active: bool,
    scores: Vec<f64>,
    tags: Vec<String>,
}

fn main() {
    let users: Vec<User> = (0..50000)
        .map(|i| User {
            id: i,
            name: format!("user {i}"),
            email: format!("user{i}@example.com"),
            active: i % 3 == 0,
            scores: vec![i as f64 * 0.5, 1.25, 99.0],
            tags: vec!["alpha".into(), "beta \"quoted\"".into()],
        })
        .collect();
    let (mut bytes, mut ids) = (0usize, 0i64);
    for _ in 0..4 {
        let text = serde_json::to_string(&users).unwrap();
        bytes += text.len();
        let back: Vec<User> = serde_json::from_str(&text).unwrap();
        ids += back.iter().map(|u| u.id).sum::<i64>();
    }
    println!("{bytes} {ids}");
}
