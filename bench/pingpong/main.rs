// Rust has no goroutines: OS threads and std's rendezvous channels.
use std::sync::mpsc::sync_channel;

fn main() {
    let (ping_tx, ping_rx) = sync_channel::<i64>(0);
    let (pong_tx, pong_rx) = sync_channel::<i64>(0);
    let t = std::thread::spawn(move || {
        for v in ping_rx {
            pong_tx.send(v + 1).unwrap();
        }
    });
    let mut v = 0;
    for _ in 0..500_000 {
        ping_tx.send(v).unwrap();
        v = pong_rx.recv().unwrap();
    }
    drop(ping_tx);
    t.join().unwrap();
    println!("{v}");
}
