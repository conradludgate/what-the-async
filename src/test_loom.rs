pub(crate) mod atomic {
    pub use loom::sync::atomic::*;
    pub use std::sync::atomic::Ordering;
}

use std::{cell::RefCell, fmt::Write};

std::thread_local! {
    static TRACE_BUF: RefCell<String> = RefCell::new(String::new());
}

pub(crate) fn traceln(args: std::fmt::Arguments) {
    let mut args = Some(args);
    TRACE_BUF
        .try_with(|buf| {
            let mut buf = buf.borrow_mut();
            let _ = buf.write_fmt(args.take().unwrap());
            let _ = buf.write_char('\n');
        })
        .unwrap_or_else(|_| println!("{}", args.take().unwrap()))
}

pub(crate) fn run_builder(
    builder: loom::model::Builder,
    model: impl Fn() + Sync + Send + std::panic::UnwindSafe + 'static,
) {
    use std::{
        env, io,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Once,
        },
    };
    use tracing_subscriber::{filter::Targets, fmt, prelude::*};

    static SETUP_TRACE: Once = Once::new();

    SETUP_TRACE.call_once(|| {
        // set up tracing for loom.
        const LOOM_LOG: &str = "LOOM_LOG";

        struct TracebufWriter;
        impl io::Write for TracebufWriter {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                let len = buf.len();
                let s = std::str::from_utf8(buf)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
                TRACE_BUF.with(|buf| buf.borrow_mut().push_str(s));
                Ok(len)
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let filter = env::var(LOOM_LOG)
            .ok()
            .and_then(|var| match var.parse::<Targets>() {
                Err(e) => {
                    eprintln!("invalid {}={:?}: {}", LOOM_LOG, var, e);
                    None
                }
                Ok(targets) => Some(targets),
            })
            .unwrap_or_else(|| Targets::new().with_target("loom", tracing::Level::INFO));
        fmt::Subscriber::builder()
            .with_writer(|| TracebufWriter)
            .without_time()
            .finish()
            .with(filter)
            .init();

        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic| {
            // try to print the trace buffer.
            TRACE_BUF
                .try_with(|buf| {
                    if let Ok(mut buf) = buf.try_borrow_mut() {
                        eprint!("{}", buf);
                        buf.clear();
                    } else {
                        eprint!("trace buf already mutably borrowed?");
                    }
                })
                .unwrap_or_else(|e| eprintln!("trace buf already torn down: {}", e));

            // let the default panic hook do the rest...
            default_hook(panic);
        }))
    });

    // wrap the loom model with `catch_unwind` to avoid potentially losing
    // test output on double panics.
    let current_iteration = std::sync::Arc::new(AtomicUsize::new(1));
    builder.check(move || {
        traceln(format_args!(
            "\n---- {} iteration {} ----",
            std::thread::current().name().unwrap_or("<unknown test>"),
            current_iteration.fetch_add(1, Ordering::Relaxed)
        ));

        model();
        // if this iteration succeeded, clear the buffer for the
        // next iteration...
        TRACE_BUF.with(|buf| buf.borrow_mut().clear());
    });
}

pub(crate) fn model(model: impl Fn() + std::panic::UnwindSafe + Sync + Send + 'static) {
    let mut builder = loom::model::Builder::default();
    // A couple of our tests will hit the max number of branches riiiiight
    // before they should complete. Double it so this stops happening.
    builder.max_branches *= 2;
    run_builder(builder, model)
}
