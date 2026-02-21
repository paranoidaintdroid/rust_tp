type Job = Box<dyn FnOnce() + Send + 'static>;
use std::{
    sync::{Arc, Mutex, mpsc, mpsc::Receiver},
    thread,
};

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

pub struct ThreadPool {
    workers: Vec<Worker>,
    sender: Option<mpsc::Sender<Job>>,
}

impl ThreadPool {
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
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread;

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

        let share_clone1 = Arc::clone(&shared);
        let share_clone2 = Arc::clone(&shared);

        let handle1 = thread::spawn(move || {
            loop {
                let message = {
                    let lock = share_clone1.lock().unwrap();
                    lock.recv()
                };

                match message {
                    Ok(msg) => {
                        println!("Worker 1 received : {}", msg);
                    }
                    Err(_) => {
                        break;
                    }
                }
            }
        });

        let handle2 = thread::spawn(move || {
            loop {
                let message = {
                    let lock = share_clone2.lock().unwrap();
                    lock.recv()
                };

                match message {
                    Ok(msg) => {
                        println!("Worker 2 received : {}", msg);
                    }
                    Err(_) => {
                        break;
                    }
                }
            }
        });

        tx.send(String::from("Job 1")).unwrap();
        tx.send(String::from("Job 2")).unwrap();
        tx.send(String::from("Job 3")).unwrap();
        tx.send(String::from("Job 4")).unwrap();

        drop(tx);

        handle1.join().unwrap();
        handle2.join().unwrap();
    }

    #[test]
    fn test_closure_as_job() {
        let (tx, rx) = mpsc::channel::<Job>();

        let dummy_str = String::from("Dummy String");

        let job: Job = Box::new(move || {
            println!("Executing job for {}", dummy_str);
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
            }).unwrap();
        }
    }
}
