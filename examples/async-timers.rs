use std::time::{Duration, Instant};

use rand::Rng;
use what_the_async as wta;
use wta::spawn_n;

fn main() {
    let runtime = wta::Runtime::default();
    let time = Instant::now();
    println!("output {:?}", runtime.block_on(start()));
    println!("took {:?}", time.elapsed());
}

async fn start() -> Duration {
    let (durs, _) = spawn_n(std::array::from_fn::<_, 10, _>(spawn_task), async {}).await;
    durs.into_iter().sum()
}

async fn spawn_task(i: usize) -> Duration {
    let ms = rand::thread_rng().gen_range(0..1000);
    let dur = Duration::from_millis(ms);
    println!(
        "task {i}, thread {thread_id:?}, dur {dur:?}",
        thread_id = std::thread::current().id()
    );
    wta::timers::Sleep::duration(dur).await;
    dur
}
