use rust_tp::PoolError;
use rust_tp::ThreadPool;

use crossbeam_channel::unbounded;

#[test]
fn test_pool_from_outside() {
    let pool = ThreadPool::build(0);
    assert!(matches!(pool, Err(PoolError::ZeroWorkers)));
    let pool = ThreadPool::build(2).unwrap();
    let (tx, rx) = unbounded();
    pool.execute(move || {
        tx.send("hello from worker").unwrap();
    })
    .unwrap();
    let received = rx.recv().unwrap();
    assert_eq!(received, "hello from worker");
}
