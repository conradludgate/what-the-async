//! This is an executor only example. Spawns 10 tasks that sleep
//! for a random duration and then return that duration.
use std::{thread, time::{Duration, Instant}};

use futures::future::join_all;
use rand::Rng;

fn main() {
    let runtime = what_the_async::Runtime::default();
    let time = Instant::now();
    dbg!(runtime.block_on(start()));
    dbg!(time.elapsed());
}

async fn start() -> Duration {
    let mut handles = vec![];
    for _ in 0..10 {
        let handle = what_the_async::spawn_blocking(|| {
            let ms = rand::thread_rng().gen_range(0..1000);
            let dur = Duration::from_millis(ms);
            thread::sleep(dbg!(dur));
            dur
        });
        handles.push(handle);
    }
    join_all(handles).await.into_iter().sum()
}
