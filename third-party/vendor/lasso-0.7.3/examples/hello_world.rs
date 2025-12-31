use lasso::ThreadedRodeo;
use std::sync::{Arc, Barrier};

fn main() {
    let thread_count = 10;
    let barrier = Arc::new(Barrier::new(thread_count));
    let interner = Arc::new(ThreadedRodeo::default());

    let mut handles = Vec::with_capacity(thread_count);
    for _ in 0..thread_count {
        let barrier = barrier.clone();
        let interner = interner.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..=100_000 {
                interner.get_or_intern(format!("Hello, world! {}", i));

                if (i % 1000) == 0 {
                    barrier.wait();
                    println!("{}", i);
                }
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }
}
