use crossbeam_channel::{Receiver, Sender, unbounded};
use std::thread;

type Job = Box<dyn FnOnce() + Send + 'static>;

/// Errors that can occur when using the ThreadPool.
///
/// This enum represents the possible failure modes when creating or using a
/// [`ThreadPool`].
///
/// # Variants
///
/// * `ZeroWorkers` — Returned when attempting to build a ThreadPool with size 0.
///   A thread pool must have at least one worker thread to execute jobs.
///
/// * `SendError` — Returned when sending a job to the ThreadPool fails.
///   This usually means the ThreadPool is shutting down and no longer accepting jobs.
#[derive(Debug, PartialEq)]
pub enum PoolError {
    ZeroWorkers,
    SendError,
}

struct Worker {
    id: usize,
    thread: Option<thread::JoinHandle<()>>,
}

impl Worker {
    fn new(id: usize, receiver: Receiver<Job>) -> Worker {
        let thread = thread::spawn(move || {
            loop {
                match receiver.recv() {
                    Ok(job) => {
                        //println!("Worker {} got a job; executing.", id);
                        job();
                    }
                    Err(_) => {
                        println!("Worker {} disconnected; shutting down.", id);
                        break;
                    }
                }
            }
        });

        Worker {
            id,
            thread: Some(thread),
        }
    }
}

/// A thread pool for executing jobs concurrently across multiple worker threads.
///
/// `ThreadPool` manages a fixed number of worker threads that wait for jobs
/// and execute them as they arrive. Jobs are closures that implement
/// `FnOnce() + Send + 'static`.
///
/// This allows efficient reuse of threads instead of spawning a new thread
/// for every task.
///
/// # Example
///
/// ```
/// use rust_tp::ThreadPool;
///
/// let pool = ThreadPool::build(4).unwrap();
///
/// pool.execute(|| {
///     println!("Hello from the thread pool!");
/// }).unwrap();
/// ```
pub struct ThreadPool {
    workers: Vec<Worker>,
    sender: Option<Sender<Job>>,
}

impl ThreadPool {
    /// Builds a new ThreadPool with the specified number of worker threads.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::ZeroWorkers`] if `size` is 0.
    ///
    /// A ThreadPool must contain at least one worker thread to function.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_tp::ThreadPool;
    ///
    /// let pool = ThreadPool::build(4);
    ///
    /// assert!(pool.is_ok());
    /// ```
    pub fn build(size: usize) -> Result<ThreadPool, PoolError> {
        if size == 0 {
            return Err(PoolError::ZeroWorkers);
        }

        let (sender, receiver) = unbounded::<Job>();

        let mut workers = Vec::with_capacity(size);

        for id in 0..size {
            workers.push(Worker::new(id, receiver.clone()));
        }

        Ok(ThreadPool {
            workers,
            sender: Some(sender),
        })
    }

    /// Executes a job on the thread pool.
    ///
    /// The job is a closure that will be executed by one of the worker threads.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::SendError`] if the ThreadPool is shutting down
    /// and can no longer accept new jobs.
    ///
    /// # Examples
    ///
    /// ```
    /// use rust_tp::ThreadPool;
    ///
    /// let pool = ThreadPool::build(2).unwrap();
    ///
    /// pool.execute(|| {
    ///     println!("Task executed");
    /// }).unwrap();
    /// ```
    pub fn execute<F>(&self, f: F) -> Result<(), PoolError>
    where
        F: FnOnce() + Send + 'static,
    {
        let job = Box::new(f);

        match &self.sender {
            Some(sender) => sender.send(job).map_err(|_| PoolError::SendError),
            None => Err(PoolError::SendError),
        }
    }
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        drop(self.sender.take());

        for worker in &mut self.workers {
            println!("Shutting down worker {}", worker.id);

            if let Some(thread) = worker.thread.take() {
                thread.join().unwrap();
            }
        }
    }
}

// TESTS

#[cfg(test)]
mod tests {
    use super::*;
    use crossbeam_channel::unbounded;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
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