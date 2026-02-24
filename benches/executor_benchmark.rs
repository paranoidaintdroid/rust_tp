use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use rust_tp::ThreadPool;

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;

type Job = Box<dyn FnOnce() + Send + 'static>;

struct NaivePool {
    _workers: Vec<thread::JoinHandle<()>>,
    sender: mpsc::Sender<Option<Job>>,
}

impl NaivePool {
    fn build(size: usize) -> Self {
        let (sender, receiver) = mpsc::channel::<Option<Job>>();
        let receiver = Arc::new(Mutex::new(receiver));
        let mut workers = Vec::with_capacity(size);

        for _ in 0..size {
            let rx = Arc::clone(&receiver);
            workers.push(thread::spawn(move || loop {
                let msg = rx.lock().unwrap().recv().unwrap();
                match msg {
                    Some(job) => job(),
                    None => break,
                }
            }));
        }

        NaivePool {
            _workers: workers,
            sender,
        }
    }

    fn execute<F: FnOnce() + Send + 'static>(&self, f: F) {
        self.sender.send(Some(Box::new(f))).unwrap();
    }
}

impl Drop for NaivePool {
    fn drop(&mut self) {
        for _ in &self._workers {
            let _ = self.sender.send(None);
        }
    }
}

fn bench_trivial_spawn(c: &mut Criterion) {
    c.bench_function("A_trivial | thread::spawn", |b| {
        b.iter(|| {
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                tx.send(black_box(1u64)).unwrap();
            });
            black_box(rx.recv().unwrap());
        })
    });
}

fn bench_trivial_naive(c: &mut Criterion) {
    let pool = NaivePool::build(4);
    c.bench_function("A_trivial | naive pool (mutex+mpsc)", |b| {
        b.iter(|| {
            let (tx, rx) = mpsc::channel();
            pool.execute(move || {
                tx.send(black_box(1u64)).unwrap();
            });
            black_box(rx.recv().unwrap());
        })
    });
}

fn bench_trivial_beast(c: &mut Criterion) {
    let pool = ThreadPool::build(4).unwrap();
    c.bench_function("A_trivial | beast pool (work-stealing)", |b| {
        b.iter(|| {
            let (tx, rx) = mpsc::channel();
            pool.execute(move || {
                tx.send(black_box(1u64)).unwrap();
            })
            .unwrap();
            black_box(rx.recv().unwrap());
        })
    });
}

fn cpu_work(n: u64) -> u64 {
    (0..n).fold(0u64, |acc, i| acc.wrapping_add(i * i))
}

fn bench_cpu_spawn(c: &mut Criterion) {
    c.bench_function("B_cpu_bound | thread::spawn", |b| {
        b.iter_batched(
            || mpsc::channel::<u64>(),
            |(tx, rx)| {
                thread::spawn(move || {
                    tx.send(black_box(cpu_work(10_000))).unwrap();
                });
                black_box(rx.recv().unwrap());
            },
            BatchSize::SmallInput,
        )
    });
}

fn bench_cpu_naive(c: &mut Criterion) {
    let pool = NaivePool::build(4);
    c.bench_function("B_cpu_bound | naive pool (mutex+mpsc)", |b| {
        b.iter_batched(
            || mpsc::channel::<u64>(),
            |(tx, rx)| {
                pool.execute(move || {
                    tx.send(black_box(cpu_work(10_000))).unwrap();
                });
                black_box(rx.recv().unwrap());
            },
            BatchSize::SmallInput,
        )
    });
}

fn bench_cpu_beast(c: &mut Criterion) {
    let pool = ThreadPool::build(4).unwrap();
    c.bench_function("B_cpu_bound | beast pool (work-stealing)", |b| {
        b.iter_batched(
            || mpsc::channel::<u64>(),
            |(tx, rx)| {
                pool.execute(move || {
                    tx.send(black_box(cpu_work(10_000))).unwrap();
                })
                .unwrap();
                black_box(rx.recv().unwrap());
            },
            BatchSize::SmallInput,
        )
    });
}

const BURST_SIZE: usize = 1_000;

fn bench_burst_spawn(c: &mut Criterion) {
    c.bench_function("C_burst | thread::spawn", |b| {
        b.iter(|| {
            let (tx, rx) = mpsc::channel();
            for _ in 0..BURST_SIZE {
                let tx = tx.clone();
                thread::spawn(move || {
                    tx.send(black_box(1u64)).unwrap();
                });
            }
            drop(tx);
            black_box(rx.iter().sum::<u64>());
        })
    });
}

fn bench_burst_naive(c: &mut Criterion) {
    let pool = NaivePool::build(4);
    c.bench_function("C_burst | naive pool (mutex+mpsc)", |b| {
        b.iter(|| {
            let (tx, rx) = mpsc::channel();
            for _ in 0..BURST_SIZE {
                let tx = tx.clone();
                pool.execute(move || {
                    tx.send(black_box(1u64)).unwrap();
                });
            }
            drop(tx);
            black_box(rx.iter().sum::<u64>());
        })
    });
}

fn bench_burst_beast(c: &mut Criterion) {
    let pool = ThreadPool::build(4).unwrap();
    c.bench_function("C_burst | beast pool (work-stealing)", |b| {
        b.iter(|| {
            let (tx, rx) = mpsc::channel();
            for _ in 0..BURST_SIZE {
                let tx = tx.clone();
                pool.execute(move || {
                    tx.send(black_box(1u64)).unwrap();
                })
                .unwrap();
            }
            drop(tx);
            black_box(rx.iter().sum::<u64>());
        })
    });
}

criterion_group!(trivial, bench_trivial_spawn, bench_trivial_naive, bench_trivial_beast);
criterion_group!(cpu_bound, bench_cpu_spawn, bench_cpu_naive, bench_cpu_beast);
criterion_group!(burst, bench_burst_spawn, bench_burst_naive, bench_burst_beast);
criterion_main!(trivial, cpu_bound, burst);