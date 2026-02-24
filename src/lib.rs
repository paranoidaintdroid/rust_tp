use core_affinity;
use crossbeam_deque::{Injector, Steal, Worker as LocalWorker};
use std::hint;
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use std::thread;
use std::thread::Thread;

type Job = Box<dyn FnOnce() + Send + 'static>;

#[derive(Debug, PartialEq)]
pub enum PoolError {
    ZeroWorkers,
    SendError,
}

#[repr(align(64))]
struct SharedWorkerState {
    task_count: AtomicUsize,
}

struct Worker {
    id: usize,
    thread: Option<thread::JoinHandle<()>>,
    state: Arc<SharedWorkerState>,
}

pub struct ThreadPool {
    workers: Vec<Worker>,
    injector: Arc<Injector<Job>>,
    shutdown: Arc<AtomicBool>,
    thread_handles: Vec<Thread>,
    next_worker: AtomicUsize,
}

impl ThreadPool {
    pub fn build(size: usize) -> Result<Self, PoolError> {
        if size == 0 {
            return Err(PoolError::ZeroWorkers);
        }

        let injector = Arc::new(Injector::new());
        let shutdown = Arc::new(AtomicBool::new(false));

        let mut workers = Vec::with_capacity(size);
        let mut thread_handles = Vec::with_capacity(size);

        let core_ids = core_affinity::get_core_ids().unwrap_or_default();

        let mut local_queues = Vec::with_capacity(size);
        let mut stealers = Vec::with_capacity(size);
        let mut states = Vec::with_capacity(size);

        for _ in 0..size {
            let local = LocalWorker::new_lifo();
            stealers.push(local.stealer());
            local_queues.push(local);
            states.push(Arc::new(SharedWorkerState {
                task_count: AtomicUsize::new(0),
            }));
        }

        for id in 0..size {
            let local = local_queues.remove(0);
            let injector_clone = Arc::clone(&injector);
            let shutdown_clone = Arc::clone(&shutdown);
            let stealers_clone = stealers.clone();
            let state_clone = Arc::clone(&states[id]);

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

                const MAX_SPINS: usize = 200;
                let mut spins = 0;

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
                        job();
                        state_clone.task_count.fetch_add(1, Ordering::Relaxed);
                        spins = 0;
                    } else {
                        if shutdown_clone.load(Ordering::Acquire) {
                            break;
                        }

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

            thread_handles.push(join_handle.thread().clone());

            workers.push(Worker {
                id,
                thread: Some(join_handle),
                state: Arc::clone(&states[id]),
            });
        }

        Ok(ThreadPool {
            workers,
            injector,
            shutdown,
            thread_handles,
            next_worker: AtomicUsize::new(0),
        })
    }

    pub fn execute<F>(&self, f: F) -> Result<(), PoolError>
    where
        F: FnOnce() + Send + 'static,
    {
        if self.shutdown.load(Ordering::Acquire) {
            return Err(PoolError::SendError);
        }

        self.injector.push(Box::new(f));

        let idx = self.next_worker.fetch_add(1, Ordering::Relaxed) % self.thread_handles.len();
        self.thread_handles[idx].unpark();

        Ok(())
    }
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);

        for handle in &self.thread_handles {
            handle.unpark();
        }

        for worker in &self.workers {
            println!(
                "Worker {} completed {} tasks",
                worker.id,
                worker.state.task_count.load(Ordering::Relaxed)
            );
        }

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