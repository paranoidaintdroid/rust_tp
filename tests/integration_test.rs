use crossbeam_channel::unbounded;
use rust_tp::PoolError;
use rust_tp::ThreadPool;
use std::time::Instant;

#[test]
fn test_pool_from_outside() {
    let pool = ThreadPool::build(0);
    assert!(matches!(pool, Err(PoolError::ZeroWorkers)));

    let pool = ThreadPool::build(4).unwrap();

    let start = Instant::now();

    for _ in 0..100_000_000u64 {
        pool.execute(|| {}).unwrap();
    }

    let submit_elapsed = start.elapsed();
    println!("Submitted 100M jobs in: {:?}", submit_elapsed);

    drop(pool);

    let total_elapsed = start.elapsed();
    println!("All 100M jobs completed in: {:?}", total_elapsed);
    println!(
        "Throughput: {:.2}M tasks/sec",
        100.0 / total_elapsed.as_secs_f64()
    );

    let pool = ThreadPool::build(4).unwrap();
    let (tx, rx) = unbounded();
    pool.execute(move || {
        tx.send("hello from worker").unwrap();
    })
    .unwrap();

    let received = rx.recv().unwrap();
    assert_eq!(received, "hello from worker");
}
