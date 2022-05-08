//! This is an full runtime example. Uses async-IO through epoll/kqueue
use std::{error::Error, net::SocketAddr};

use futures::{io::BufReader, AsyncBufReadExt, AsyncWriteExt, StreamExt};
use wta_reactor::net::{TcpListener, TcpStream};

fn main() -> Result<(), Box<dyn Error + Send + Sync + 'static>> {
    let runtime = what_the_async::Runtime::default();
    runtime.block_on(start())
}

async fn start() -> Result<(), Box<dyn Error + Send + Sync + 'static>> {
    let addr = "127.0.0.1:8080";
    let server = TcpListener::bind(addr.parse().unwrap())?;
    println!("Listening on: {}", addr);

    let mut accept = server.accept();

    loop {
        let (stream, addr) = accept.next().await.unwrap()?;
        what_the_async::spawn(async move {
            if let Err(e) = process(stream, addr).await {
                println!("failed to process connection; error = {}", e);
            }
        });
    }
}

async fn process(
    stream: TcpStream,
    addr: SocketAddr,
) -> Result<(), Box<dyn Error + Send + Sync + 'static>> {
    println!(
        "connected to addr {addr:?} on thread {thread_id:?}",
        thread_id = std::thread::current().id()
    );

    let mut reader = BufReader::new(stream);

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            break;
        };
        println!(
            "read {line:?} from addr {addr:?} on thread {thread_id:?}",
            thread_id = std::thread::current().id()
        );

        reader.get_mut().write_all(line.as_bytes()).await?;

        println!(
            "wrote to addr {addr:?} on thread {thread_id:?}",
            thread_id = std::thread::current().id()
        );
    }

    println!(
        "closed conn to addr {addr:?} on thread {thread_id:?}",
        thread_id = std::thread::current().id()
    );

    Ok(())
}
