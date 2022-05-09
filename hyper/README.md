# Hyper

[Hyper](https://crates.io/crates/hyper/) is a low-level HTTP library that normally makes use of tokio,
but is fundamentally runtime agnostic. We can use hyper over our runtime
by implementing the [Executor trait](https://docs.rs/hyper/0.14.18/hyper/rt/trait.Executor.html)
and the [Accept trait](https://docs.rs/hyper/0.14.18/hyper/server/accept/trait.Accept.html).
