//! This is an full runtime example. Runs the TcpListener through hyper
use std::{convert::Infallible, net::SocketAddr, time::Duration};

use hyper::{
    server::Server,
    service::{make_service_fn, service_fn},
    Body, Request, Response,
};
use what_the_async::timers::Sleep;
use wta_hyper::{AddrStream, Executor, Incoming};

fn main() {
    let mut runtime = what_the_async::Runtime::default();
    runtime.block_on(start())
}

async fn start() {
    let make_service = make_service_fn(move |conn: &AddrStream| {
        let addr = conn.remote_addr();
        let service = service_fn(move |req| handle(addr, req));
        async move { Ok::<_, Infallible>(service) }
    });

    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));

    let server = Server::builder(Incoming::bind(addr).unwrap())
        .executor(Executor)
        .serve(make_service);

    server.await.unwrap();
}

async fn handle(_addr: SocketAddr, _req: Request<Body>) -> Result<Response<Body>, Infallible> {
    use rand::Rng;
    let ms = rand::thread_rng().gen_range(50..100);
    Sleep::duration(Duration::from_millis(ms)).await;

    Ok(Response::new(Body::from("Hello World")))
}
