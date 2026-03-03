# rust_tp — Work-Stealing Thread Pool

A thread pool executor that started naive (Mutex + mpsc) and got fast.
What did I do? Replaced the single shared queue with `crossbeam_deque`'s work-stealing architecture, each worker now has its own local job queue, and only reaches for a neighbour's queue when its own runs dry.

---

## What's inside

- **Lock-free job submission** via `crossbeam_deque`'s `Injector`
- **Work-stealing** - idle workers steal from busy workers' local queues
- **Core pinning** via `core_affinity` - worker N stays on core N, cache stays hot
- **Spin-then-park backoff** - spins 200 times (emitting a CPU PAUSE hint), then parks the OS thread at zero CPU cost
- **False-sharing prevention** - per-worker counters are `#[repr(align(64))]` padded to their own cache lines
- **Clean shutdown** - dropping the pool blocks until every submitted job finishes

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

The naive approach : every worker fights over one `Mutex<Receiver>` — turns the queue into a bottleneck. Under load, workers spend more time waiting for the lock than doing actual work. Work-stealing flips this: each worker owns its queue, contention only happens when a worker is idle and reaching out to steal. The hot path (your own local queue) has zero contention.

---

## Benchmarks

Three groups, each comparing `thread::spawn` vs the naive `Mutex+mpsc` pool vs this work-stealing pool.

**Test machine:** AMD Ryzen 5 7530U @ 2.00 GHz · 16 GB RAM · Windows 11 x64

| Group | What it measures |
|---|---|
| `A_trivial` | Raw scheduling overhead — single no-op job |
| `B_cpu_bound` | Throughput under real load — single CPU-heavy job |
| `C_burst` | Queue throughput — 1,000 jobs submitted at once |

### Results

#### A — Trivial (single no-op job)

| Executor | Median latency | vs. work-stealing |
|---|---|---|
| `thread::spawn` | 301.19 µs | ~127× slower |
| Naive pool (Mutex+mpsc) | 28.50 µs | ~12× slower |
| **Our Thread pool (work-stealing)** | **2.38 µs** | — |

Spawning a raw OS thread takes ~300 µs on this machine. The naive pool cuts that to ~28 µs by reusing threads, but the Mutex still hurts. Work-stealing drops to **2.4 µs** — essentially free once the pool is warm.

#### B — CPU-bound (single heavy job, 10 000-iteration fold)

| Executor | Median latency | vs. work-stealing |
|---|---|---|
| `thread::spawn` | 226.02 µs | ~100× slower |
| Naive pool (Mutex+mpsc) | 29.50 µs | ~13× slower |
| **Our Thread pool (work-stealing)** | **2.25 µs** | — |

Dispatch overhead stays flat at ~2.25 µs regardless of job weight — the pool is not the bottleneck.

Worker task distribution across the B_cpu_bound run:
```
Worker 0: 1 125 860 tasks
Worker 1: 1 188 113 tasks
Worker 2:   965 412 tasks
Worker 3: 1 034 716 tasks
```
Reasonably balanced; no single worker is starved.

#### C — Burst (1 000 jobs submitted at once)

| Executor | Median time | vs. work-stealing |
|---|---|---|
| `thread::spawn` | 111.42 ms | ~220× slower |
| Naive pool (Mutex+mpsc) | 459.01 µs | ~0.9× (slightly faster) |
| **Our Thread pool (work-stealing)** | **507.16 µs** | — |

`thread::spawn` is catastrophically slow here — spawning 1 000 OS threads costs ~111 ms. Both pools process the burst in under a millisecond.

The work-stealing pool is marginally slower (~10%) than naive in the burst case. This is expected: with 1 000 tiny no-op jobs, the cost of each job is dominated by the channel round-trip used to measure completion, not actual CPU work. The injector/steal path adds a small constant overhead that only shows when per-job work approaches zero. Under any real workload (see groups A and B) the work-stealing pool wins decisively.

Worker task distribution across the C_burst run:
```
Worker 0: 4 592 996 tasks
Worker 1: 5 262 133 tasks
Worker 2: 3 997 655 tasks
Worker 3: 4 438 216 tasks
```

### Running the benchmarks yourself

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