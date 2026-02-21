use std::{
    sync::{Arc, Mutex, mpsc, mpsc::Receiver},
    thread,
};

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
    fn new(id: usize, receiver: Arc<Mutex<Receiver<Job>>>) -> Worker {
        let thread = thread::spawn(move || {
            loop {
                let message = match receiver.lock() {
                    Ok(lock) => lock.recv(),
                    Err(_) => {
                        println!("Worker {} mutex poisoned; shutting down.", id);
                        break;
                    }
                };

                match message {
                    Ok(job) => {
                        println!("Worker {} got a job; executing.", id);
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
    sender: Option<mpsc::Sender<Job>>,
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

        let (sender, receiver) = mpsc::channel::<Job>();
        let receiver = Arc::new(Mutex::new(receiver));

        let mut workers = Vec::with_capacity(size);

        for id in 0..size {
            workers.push(Worker::new(id, Arc::clone(&receiver)));
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
        let job: Job = Box::new(f);

        let sender = self.sender.as_ref().ok_or(PoolError::SendError)?;

        sender.send(job).map_err(|_| PoolError::SendError)?;

        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};

    #[test]
    fn test_mpsc_basics() {
        let (tx, rx) = mpsc::channel::<String>();

        let tx1 = tx.clone();
        let tx2 = tx.clone();

        let handle1 = thread::spawn(move || {
            tx1.send(String::from("Thread 1 reporting")).unwrap();
        });

        let handle2 = thread::spawn(move || {
            tx2.send(String::from("Thread 2 reporting")).unwrap();
        });

        drop(tx);

        for received in rx {
            println!("{}", received);
        }

        handle1.join().unwrap();
        handle2.join().unwrap();
    }

    #[test]
    fn test_arc_mutex_receiver() {
        let (tx, rx) = mpsc::channel::<String>();
        let shared = Arc::new(Mutex::new(rx));

        let s1 = Arc::clone(&shared);
        let s2 = Arc::clone(&shared);

        let handle1 = thread::spawn(move || {
            loop {
                let message = {
                    let lock = s1.lock().unwrap();
                    lock.recv()
                };

                match message {
                    Ok(msg) => println!("Worker 1 received: {}", msg),
                    Err(_) => break,
                }
            }
        });

        let handle2 = thread::spawn(move || {
            loop {
                let message = {
                    let lock = s2.lock().unwrap();
                    lock.recv()
                };

                match message {
                    Ok(msg) => println!("Worker 2 received: {}", msg),
                    Err(_) => break,
                }
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
        let (tx, rx) = mpsc::channel::<Job>();

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
