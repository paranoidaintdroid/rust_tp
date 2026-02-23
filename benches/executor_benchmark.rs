use criterion::{Criterion, black_box, criterion_group, criterion_main};
use rust_tp::ThreadPool;

use std::sync::mpsc;
use std::thread;

fn benchmark_thread_spawn(c: &mut Criterion) {
    c.bench_function("thread_spawn_single_job", |b| {
        b.iter(|| {
            let (tx, rx) = mpsc::channel();

            thread::spawn(move || {
                tx.send(black_box(42)).unwrap();
            });

            black_box(rx.recv().unwrap());
        })
    });
}

fn benchmark_threadpool_execution(c: &mut Criterion) {
    let pool = ThreadPool::build(4).unwrap();

    c.bench_function("threadpool_single_job", |b| {
        b.iter(|| {
            let (tx, rx) = mpsc::channel();

            pool.execute(move || {
                tx.send(black_box(42)).unwrap();
            })
            .unwrap();

            black_box(rx.recv().unwrap());
        })
    });
}

fn executor_benchmark(c: &mut Criterion) {
    benchmark_thread_spawn(c);
    benchmark_threadpool_execution(c);
}

criterion_group!(benches, executor_benchmark);
criterion_main!(benches);
