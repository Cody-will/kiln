// OS threads; std's mpsc receiver is shared through a Mutex (no MPMC in std).
use std::sync::mpsc::sync_channel;
use std::sync::{Arc, Mutex};

fn collatz(mut n: i64) -> i64 {
    let mut steps = 0;
    while n != 1 {
        n = if n % 2 == 0 { n / 2 } else { 3 * n + 1 };
        steps += 1;
    }
    steps
}

fn main() {
    let (jobs_tx, jobs_rx) = sync_channel::<i64>(1024);
    let (res_tx, res_rx) = sync_channel::<i64>(1024);
    let jobs_rx = Arc::new(Mutex::new(jobs_rx));
    let workers: Vec<_> = (0..8)
        .map(|_| {
            let rx = Arc::clone(&jobs_rx);
            let tx = res_tx.clone();
            std::thread::spawn(move || loop {
                let j = rx.lock().unwrap().recv();
                match j {
                    Ok(j) => tx.send(collatz(j)).unwrap(),
                    Err(_) => break,
                }
            })
        })
        .collect();
    drop(res_tx);
    std::thread::spawn(move || {
        for i in 1..=200_000i64 {
            jobs_tx.send(i).unwrap();
        }
    });
    let total: i64 = res_rx.iter().sum();
    for w in workers {
        w.join().unwrap();
    }
    println!("{total}");
}
