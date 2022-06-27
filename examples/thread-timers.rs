//! This is an executor only example. Spawns 10 tasks that sleep
//! for a random duration and then return that duration.
use std::{
    thread,
    time::{Duration, Instant},
};

use futures::future::join_all;
use rand::Rng;

fn main() {
    let mut runtime = what_the_async::Runtime::default();
    let time = Instant::now();
    println!("output {:?}", runtime.block_on(start()));
    println!("took {:?}", time.elapsed());
}

async fn start() -> Duration {
    let mut handles = vec![];
    for i in 0..10 {
        let handle = spawn_task(i);
        handles.push(handle);
    }
    join_all(handles).await.into_iter().sum()
}

async fn spawn_task(i: usize) -> Duration {
    what_the_async::spawn_blocking(move || {
        let ms = rand::thread_rng().gen_range(0..1000);
        let dur = Duration::from_millis(ms);
        println!(
            "task {i}, thread {thread_id:?}, dur {dur:?}",
            thread_id = std::thread::current().id()
        );
        thread::sleep(dur);
        dur
    })
    .await
}
