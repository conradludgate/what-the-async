//! This is an full runtime example. Spawns 10 tasks that sleep
//! for a random duration and then return that duration.
use std::time::{Duration, Instant};

use futures::future::join_all;
use rand::Rng;
use wta_reactor::timers::Sleep;

fn main() {
    let runtime = what_the_async::Runtime::default();
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
    what_the_async::spawn(async move {
        let ms = rand::thread_rng().gen_range(0..1000);
        let dur = Duration::from_millis(ms);
        println!(
            "task {i}, thread {thread_id:?}, dur {dur:?}",
            thread_id = std::thread::current().id()
        );
        Sleep::duration(dur).await;
        dur
    })
    .await
}
