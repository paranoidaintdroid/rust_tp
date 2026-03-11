// main.rs
use rust_tp::ThreadPool;
use crossbeam_channel::unbounded;

fn main() {
    let pool = ThreadPool::build(4).unwrap();
    let (tx, rx) = unbounded();

    for i in 0..50 {
        let tx = tx.clone();
        pool.execute(move || {
            let result = i * i;
            tx.send((i, result)).unwrap();
        }).unwrap();
    }

    drop(tx); 

    for (i, result) in rx.iter() {
        println!("Task {}: {} * {} = {}", i, i, i, result);
    }
}
