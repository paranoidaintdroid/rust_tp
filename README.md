# rust_tp — Work-Stealing Thread Pool

A thread pool executor that started naive (Mutex + mpsc) and got fast.
The key upgrade? Replaced the single shared queue with `crossbeam_deque`'s work-stealing architecture — each worker now has its own local job queue, and only reaches for a neighbour's queue when its own runs dry.

---

## What's inside

- **Lock-free job submission** via `crossbeam_deque`'s `Injector`
- **Work-stealing** — idle workers steal from busy workers' local queues
- **Core pinning** via `core_affinity` — worker N stays on core N, cache stays hot
- **Spin-then-park backoff** — spins 200 times (emitting a CPU PAUSE hint), then parks the OS thread at zero CPU cost
- **False-sharing prevention** — per-worker counters are `#[repr(align(64))]` padded to their own cache lines
- **Clean shutdown** — dropping the pool blocks until every submitted job finishes

---

## Quick start

```rust
use rust_tp::ThreadPool;

let pool = ThreadPool::build(4).unwrap();

for i in 0..8 {
    pool.execute(move || println!("job {i}")).unwrap();
}

// drop blocks here until all 8 jobs are done
```

Getting a result back from a worker:

```rust
use rust_tp::ThreadPool;
use crossbeam_channel::unbounded;

let pool = ThreadPool::build(4).unwrap();
let (tx, rx) = unbounded();

pool.execute(move || tx.send(42u32).unwrap()).unwrap();

let result = rx.recv().unwrap(); // 42
```

---

## Errors

```rust
use rust_tp::{ThreadPool, PoolError};

// size 0 makes no sense — rejected immediately
assert_eq!(ThreadPool::build(0), Err(PoolError::ZeroWorkers));

// submitting after drop returns SendError
let pool = ThreadPool::build(4).unwrap();
drop(pool);
// pool.execute(|| {}) would return Err(PoolError::SendError) here
```

---

## How the worker loop works

Each worker runs the same four-step search on every iteration:

```
STEP 1: Check own local queue  ->  found? run it
STEP 2: Steal from Injector    ->  found? run it
STEP 3: Steal from peers       ->  found? run it
STEP 4: Nothing found?
        └─ shutdown flag set?  ->  EXIT
        └─ spins < 200?        ->  spin_loop hint, try again
        └─ spins >= 200?       ->  park(), wait for unpark() from execute()
```

---

## Why not a single shared Mutex\<Queue\>?

The naive approach — every worker fights over one `Mutex<Receiver>` — turns the queue into a bottleneck. Under load, workers spend more time waiting for the lock than doing actual work. Work-stealing flips this: each worker owns its queue, contention only happens when a worker is idle and reaching out to steal. The hot path (your own local queue) has zero contention.

---

## Benchmarks

Three groups, each comparing `thread::spawn` vs the naive `Mutex+mpsc` pool vs this work-stealing pool:

| Group | What it measures |
|---|---|
| `A_trivial` | Raw scheduling overhead — single no-op job |
| `B_cpu_bound` | Throughput under real load — single CPU-heavy job |
| `C_burst` | Queue throughput — 1,000 jobs submitted at once |

```bash
cargo bench
```

---

## Dependencies

```toml
[dependencies]
crossbeam-channel = "0.5"
crossbeam-deque   = "0.8"
core_affinity     = "0.8"
```

Rust edition 2024.
