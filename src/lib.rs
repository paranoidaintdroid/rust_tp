// `core_affinity` is an external crate that helps us discover the logical CPU cores
// available on the machine and pin individual threads to specific cores. (Thread 1
// should work with core 1 only)
//
// Why? By default, the OS scheduler is free to migrate a thread
// from core 0 to core 3 at any moment.
// Pinning worker N permanently to core N keeps its cache hot.
use core_affinity;

// `crossbeam_deque` provides the three-part work-stealing data structure, on which our
// Thread Pool Exceutor's performance depends
//
// - `Injector<T>` is the global entry point. Any thread can push jobs onto it.
//   It is a lock-free multi-producer queue.
//
// - `Steal<T>` is the return type of every steal attempt. It is a three-variant enum:
//   `Success(job)` means you got a job, `Empty` means the queue had nothing,
//   and `Retry` means you collided and must try again.
//
// - `Worker<T>` (aliased here as `LocalWorker` to avoid clashing with our own
//   `Worker` struct below) is the thread-local end of a work-stealing deque.
//   Only the owning thread pushes and pops from it. Other threads steal from
//   the opposite end via `Stealer<T>` handles.
use crossbeam_deque::{Injector, Steal, Worker as LocalWorker};

// `std::hint` gives/hints low-level suggestions to the compiler and CPU about
// the intent of our code.
use std::hint;

// `std::sync` is the standard library's module that allow multiple threads to
// safely coordinate access to shared state.
//
// From it we pull three things:
//
// `AtomicBool` - a boolean whose load and store operations are guaranteed to be
// indivisible even under concurrent access from many threads.
//
// `AtomicUsize` - an unsigned integer of pointer size with the same atomicity
// guarantee.
//
// `Ordering` - the enum that controls how much synchronization each atomic
// operation performs.
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// `Arc<T>` - Atomically Reference Counted is a heap-allocated smart pointer
// that allows *multiple owners* of the same data.
// `Arc::clone(&x)`, helps you *atomically increment a reference count*.
// When the last `Arc`dropped, the reference count hits zero and the value is freed.
use std::sync::Arc;

// `std::thread` is the module for spawning and managing OS threads.
use std::thread;

// `std::thread::Thread` - note the capital T? is a *handle* to a specific OS
// thread that is already running. It is different from `JoinHandle`, which represents
// ownership of a thread and is consumed by `.join()`.
use std::thread::Thread;

// `tpye` - it creates a new name for an existing type.
// so everytime compiler sees `Job`, it equates to
// ` Box<dyn FnOnce() + Send + 'static>`
//
// `Box<>` - Before we get into why Box is used here, we must know in rust every
// value must have known size at compile time. But dyn FnOnce() is not sized
// So Box here turns closure of unknown size into know sized pointer.
//
// `dyn` - runtime polymorphism, it stands for dynamic dispatch
//
// `FnOnce()` - A trait representing callable things that can only be called once
// it is a closure trait (FnOnce > FnMut > Fn), so it can accept any closure
//
// `Send` - A type is Send if it is safe to transfer ownership to another thread.
// `'static` - Shouldnt borrow anything that might be dropped while value still
//  exists.
//
// Now both of them are added to Trait Object using `+`.
type Job = Box<dyn FnOnce() + Send + 'static>;

// `#[derive(x,y)]` - basically means, hey rust, write the x and y trait
// implementation for me, "Auto-generate this boring implementation for me."
//
// `Debug` - lets us print the type with `{:?}`
// `ParialEq` - generates an == and != implementation, we cant directly check
// if the enum variants are equal or not without this.
//
/// `pub enum PoolError` - a public enum, and `ZeroWorkers` and `SendError` are
/// unit variants of it (contain no additional data).
#[derive(Debug, PartialEq)]
pub enum PoolError {
    /// Returned by [`ThreadPool::build`] when `size` is `0`.
    /// A pool with no workers can never make progress, so we reject it upfront.
    ZeroWorkers,
    /// Returned by [`ThreadPool::execute`] when the pool is already shutting down
    /// and can no longer accept new jobs.
    SendError,
}

// `#[repr(align(64))]` - Each SharedWorkerState should start at memory address
// that is multiple of 64, why though? Because CPU cache lines are 64 bytes on
// most modern machines. Huh? so when CPU reads a memory, it reads 64 bytes at once.
//
// If we had not used this, there would have been a case where all counters would
// have been packed in one Cache Line like this :
// ┌────────────────────────────────────────────────────┐
// │ counter0 | counter1 | counter2 | counter3 | ...    │
// └────────────────────────────────────────────────────┘
// So, when Worker1 from Core tries to increment counter 1, other Cores should
// drop the copy of this particular Cache Line, and they would need to bring it
// back from L3. {Hence a silent performance issue}
//
// After `#[repr(align(64))]`, the memory layout looks like this, each counter
// has its own cache line, so there is no fight between Cores.
//
// ┌────────────────────────────────────────┐
// │ counter0 (8B) | padding (56B)          │
// └────────────────────────────────────────┘
// ┌────────────────────────────────────────┐
// │ counter1 (8B) | padding (56B)          │
// └────────────────────────────────────────┘
//
// `#[repr(align(64))]` doesnt work on individual Variables, so we had to wrap
// our AtomicUsize into a Struct (Type Definition).
#[repr(align(64))]
struct SharedWorkerState {
    task_count: AtomicUsize,
}

// `struct Worker`
// `id: usize` - Nothing special, it used for just indexing
//
// `thread: Option<thread::JoinHandle<()>>` - Why Option<> enum? it has two variants
// Some<T> and None, when you call .take() on a mutable reference of Some<T>
// it replace Some<T> with None, gives us the value of it back. Why do we need the value?
// Since .join() on JoinHandle consumes it, the handle is gone, though we need it for shutdown.
//
// `state: Arc<SharedWorkerState>` - The Worker struct needs it to read the task_count when
// printing stats during shutdown. The spawned thread needs it to increment task_count after each job.
// Neither owns the SharedWorkerState they share it using Arc.
struct Worker {
    id: usize,
    thread: Option<thread::JoinHandle<()>>,
    state: Arc<SharedWorkerState>,
}

/// `pub struct ThreadPool` - A control room basically, it lets the fields inside it co-ordinate.
///
/// Create one with [`ThreadPool::build`], submit work with [`ThreadPool::execute`].
/// Dropping the pool blocks until every submitted job has finished — no silent abandonment.
// `workers: Vec<Worker>` - It holds all the Workers we need.
//
// `injector: Arc<Injector<Job>>` - Injector?  the front door through which all external work enters
// the system before being distributed to workers So, why Arc here? because it needs to be shared
// between threads, how else are we gonna inject?
//
// `shutdown: Arc<AtomicBool>` - So Arc again, for reusability, and the field? as the name suggests.
// It is used to set a flag and let workers notice it and then start shutting down.
//
// `thread_handles: Vec<Thread>` - We have stored them seperately, so each time we wouldnt have to get
// thread reference via JoinHandle, it saves time.
//
// `next_worker: AtomicUsize` - Used in round robin, just to know which worker to wake up next.
pub struct ThreadPool {
    workers: Vec<Worker>,
    injector: Arc<Injector<Job>>,
    shutdown: Arc<AtomicBool>,
    thread_handles: Vec<Thread>,
    next_worker: AtomicUsize,
}

// `impl ThreadPool` - This is where we define the methods associated with our struct.
impl ThreadPool {
    /// We haven't named it "new", since there is a possibility of failing, so we named it something
    /// else, like "build" or "try_new". Our pool can fail when size == 0 !
    ///
    /// `Result<Self, PoolError>` - this is the return type of our build method, its an enum Result<T,E>
    /// T returns the value we expect when our build method runs successfully else it returns E (Error).
    /// So either Ok(ThreadPool) or Err(PoolError).
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::ZeroWorkers`] if `size` is `0`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use rust_tp::{ThreadPool, PoolError};
    ///
    /// // size 0 is rejected
    /// assert_eq!(ThreadPool::build(0), Err(PoolError::ZeroWorkers));
    ///
    /// // size 4 gives us a ready-to-use pool
    /// let pool = ThreadPool::build(4).unwrap();
    /// ```
    pub fn build(size: usize) -> Result<Self, PoolError> {
        // The pool size cant be Zero, "group of people, but there is no one in the group"
        if size == 0 {
            // returns Err(E), where E is our own defined Error.
            return Err(PoolError::ZeroWorkers);
        }

        // Initializing the stuff we need
        // The global job queue where all submitted jobs land first, shared between the main thread and all workers via Arc
        let injector = Arc::new(Injector::new());
        // a single atomic boolean flag, starts false, flipped to true when the pool is dropped to tell all workers to stop.
        let shutdown = Arc::new(AtomicBool::new(false));
        //permanent records of each worker, holds the JoinHandle and state reference, lives in ThreadPool until shutdown.
        let mut workers = Vec::with_capacity(size);
        // lightweight wakeup handles used by execute to unpark sleeping workers.
        let mut thread_handles = Vec::with_capacity(size);
        // list of logical CPU cores on this machine, used to pin each worker thread to a dedicated core.
        let core_ids = core_affinity::get_core_ids().unwrap_or_default();
        // each worker's own private job queue.
        let mut local_queues = Vec::with_capacity(size);
        // read-only handles into each worker's local queue, cloned and given to every other worker so they can steal jobs.
        let mut stealers = Vec::with_capacity(size);
        //  per-worker task counters wrapped in Arc
        let mut states = Vec::with_capacity(size);

        // Preparation Loop
        for _ in 0..size {
            // Crossbeam deque
            let local = LocalWorker::new_lifo();
            // local.stealer() - returns a read only handle to opposite end of crossbeam deque (local here)
            // then push em into the stealer Vec
            stealers.push(local.stealer());
            // local is pushed into local_queues (Each worker having its own queue)
            local_queues.push(local);
            // States have been initialized with Zero
            states.push(Arc::new(SharedWorkerState {
                task_count: AtomicUsize::new(0),
            }));
        }

        // local_queues: [LW0,    LW1,    LW2,    LW3   ]  ← will be moved into threads
        // stealers:     [S0,     S1,     S2,     S3    ]  ← will be cloned into every thread
        // states:       [Arc<SW0>, Arc<SW1>, Arc<SW2>, Arc<SW3>]  ← shared between ThreadPool and threads
        // workers:      []  ← still empty, filled in spawning loop
        // thread_handles: []  ← still empty, filled in spawning loop

        for id in 0..size {
            //ownership of the queue from local_queues is transferred to local
            let local = local_queues.remove(0);
            //Injector remains here, while clones get distributed to workers
            let injector_clone = Arc::clone(&injector);
            //Shutdown remains here, while clones get distributed to workers
            let shutdown_clone = Arc::clone(&shutdown);
            //stealers_clone is a full copy of the Vec of stealer handles — moves into this worker's closure.
            let stealers_clone = stealers.clone();
            //to move into worker closure, like all of the above
            let state_clone = Arc::clone(&states[id]);

            //Two distinct things happening here. First, deciding which core this worker should run on.
            //Second, actually spawning the OS thread and immediately pinning it,else run em unpinned.
            let target_core = if core_ids.is_empty() {
                None
            } else {
                Some(core_ids[id % core_ids.len()])
            };

            let join_handle = thread::spawn(move || {
                if let Some(core_id) = target_core {
                    if !core_affinity::set_for_current(core_id) {
                        eprintln!("Worker {}: affinity unavailable, running unpinned", id);
                    }
                }
                // Max spins our code is allowed to do to keep our cores wAEM
                const MAX_SPINS: usize = 200;
                let mut spins = 0;

                //   STEP 1: Check own local queue first
                //           │
                //           ├─ found job? -> run it -> increment task_count -> loop again
                //           │
                //           └─ empty? -> go to STEP 2

                //   STEP 2: Steal from global Injector
                //           │
                //           ├─ Success(job) -> run it -> increment task_count -> loop again
                //           ├─ Retry        -> try injector again (collision, another worker grabbed it)
                //           │
                //           └─ Empty -> go to STEP 3

                //   STEP 3: Steal from other workers' local queues
                //           │
                //           ├─ Success(job) -> run it -> increment task_count -> loop again
                //           ├─ Retry        -> try that stealer again
                //           │
                //           └─ all empty -> go to STEP 4

                //   STEP 4: No work found anywhere
                //           │
                //           ├─ shutdown flag set? -> EXIT LOOP, thread finishes
                //           │
                //           ├─ spins < 200?  -> spin_loop hint (stay hot, burns CPU briefly)
                //           │                    spins++, loop again from STEP 1
                //           │
                //           └─ spins >= 200? -> thread::park() (sleep, 0 CPU, wait for unpark)
                //                                when unparked, reset spins, loop again from STEP 1

                let find_task = || -> Option<Job> {
                    if let Some(job) = local.pop() {
                        return Some(job);
                    }

                    loop {
                        match injector_clone.steal() {
                            Steal::Success(job) => return Some(job),
                            Steal::Empty => break,
                            Steal::Retry => continue,
                        }
                    }

                    for stealer in &stealers_clone {
                        loop {
                            match stealer.steal() {
                                Steal::Success(job) => return Some(job),
                                Steal::Empty => break,
                                Steal::Retry => continue,
                            }
                        }
                    }

                    None
                };

                loop {
                    if let Some(job) = find_task() {
                        // This is the actual execution of the user's closure.
                        // Whatever the user submitted via execute, a computation, a network call, a file write
                        // happens right here on this line.
                        job();
                        //incrementing the task count
                        state_clone.task_count.fetch_add(1, Ordering::Relaxed);
                        // Spin is zero, since our worker is active
                        spins = 0;
                    } else {
                        // Check if our ThreadPool is shutting down (since no Job)
                        if shutdown_clone.load(Ordering::Acquire) {
                            break;
                        }

                        // keep worker active and Core warm, until 200 spins are done
                        // after that park the thread and reset spin counter
                        if spins < MAX_SPINS {
                            hint::spin_loop();
                            spins += 1;
                        } else {
                            thread::park();
                            spins = 0;
                        }
                    }
                }
            });

            // pushing Threads into our thread_handles vec (to call unpark on them)
            thread_handles.push(join_handle.thread().clone());

            //pushing worker into worker vec
            workers.push(Worker {
                id,
                thread: Some(join_handle),
                state: Arc::clone(&states[id]),
            });
        }

        // finally returning the threadpool
        Ok(ThreadPool {
            workers,
            injector,
            shutdown,
            thread_handles,
            next_worker: AtomicUsize::new(0),
        })
    }

    /// Submits a closure to be executed by the thread pool.
    ///
    /// The closure is type-erased, heap-allocated, and pushed onto the lock-free
    /// global job queue. One worker thread is then woken via round-robin to go
    /// claim it. Submission is wait-free — no locks are acquired.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::SendError`] if the pool is shutting down.
    ///
    /// # Example
    ///
    /// ```rust
    /// use rust_tp::ThreadPool;
    ///
    /// let pool = ThreadPool::build(4).unwrap();
    /// let (tx, rx) = crossbeam_channel::unbounded();
    /// pool.execute(move || tx.send(42u32).unwrap()).unwrap();
    /// assert_eq!(rx.recv().unwrap(), 42);
    /// ```
    pub fn execute<F>(&self, f: F) -> Result<(), PoolError>
    where
        F: FnOnce() + Send + 'static,
    {
        // Shutdown check
        if self.shutdown.load(Ordering::Acquire) {
            return Err(PoolError::SendError);
        }

        // Insert the job into our global queue
        self.injector.push(Box::new(f));

        //Wake up threads in Round Robin Manner
        let idx = self.next_worker.fetch_add(1, Ordering::Relaxed) % self.thread_handles.len();
        self.thread_handles[idx].unpark();

        // returns an unit, not meaningful
        Ok(())
    }
}

impl Drop for ThreadPool {
    fn drop(&mut self) {

        // set the shutdown flag
        self.shutdown.store(true, Ordering::Release);

        // wake up all threads, so they can check {Is Shutdown Happening?}
        for handle in &self.thread_handles {
            handle.unpark();
        }

        // Print the Task Count
        for worker in &self.workers {
            println!(
                "Worker {} completed {} tasks",
                worker.id,
                worker.state.task_count.load(Ordering::Relaxed)
            );
        }

        //Join all Workers
        for worker in &mut self.workers {
            if let Some(thread) = worker.thread.take() {
                thread.join().unwrap();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn test_channel_basics() {
        let (tx, rx) = unbounded::<String>();
        let tx1 = tx.clone();
        let tx2 = tx.clone();

        let handle1 = thread::spawn(move || {
            tx1.send(String::from("Thread 1 reporting")).unwrap();
        });
        let handle2 = thread::spawn(move || {
            tx2.send(String::from("Thread 2 reporting")).unwrap();
        });

        drop(tx);
        for received in rx.iter() {
            println!("{}", received);
        }

        handle1.join().unwrap();
        handle2.join().unwrap();
    }

    #[test]
    fn test_multiple_receivers_without_mutex() {
        let (tx, rx) = unbounded::<String>();
        let rx1 = rx.clone();
        let rx2 = rx.clone();

        let handle1 = thread::spawn(move || {
            while let Ok(msg) = rx1.recv() {
                println!("Worker 1 received: {}", msg);
            }
        });
        let handle2 = thread::spawn(move || {
            while let Ok(msg) = rx2.recv() {
                println!("Worker 2 received: {}", msg);
            }
        });

        tx.send("Job 1".to_string()).unwrap();
        tx.send("Job 2".to_string()).unwrap();
        tx.send("Job 3".to_string()).unwrap();
        tx.send("Job 4".to_string()).unwrap();
        drop(tx);

        handle1.join().unwrap();
        handle2.join().unwrap();
    }

    #[test]
    fn test_closure_as_job() {
        let (tx, rx) = unbounded::<Job>();
        let msg = String::from("Dummy String");
        let job: Job = Box::new(move || {
            println!("Executing job for {}", msg);
        });

        tx.send(job).unwrap();
        let received_job = rx.recv().unwrap();
        received_job();
    }

    #[test]
    fn test_threadpool_execution() {
        let pool = ThreadPool::build(4).unwrap();
        for i in 0..8 {
            pool.execute(move || {
                println!("Executing job {} from test", i);
            })
            .unwrap();
        }
        drop(pool);
    }

    #[test]
    fn test_proves_concurrency() {
        let pool = ThreadPool::build(4).unwrap();
        let counter = Arc::new(AtomicUsize::new(0));
        let start = Instant::now();

        for _ in 0..4 {
            let counter_clone = Arc::clone(&counter);
            pool.execute(move || {
                thread::sleep(Duration::from_millis(250));
                counter_clone.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();
        }

        drop(pool);
        let elapsed = start.elapsed();

        assert_eq!(counter.load(Ordering::SeqCst), 4);
        assert!(
            elapsed < Duration::from_millis(400),
            "Jobs did not run concurrently. Elapsed: {:?}",
            elapsed
        );
    }
}