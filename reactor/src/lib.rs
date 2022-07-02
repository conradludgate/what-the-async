#![forbid(unsafe_code)]
#![warn(clippy::pedantic)]
#![allow(clippy::missing_errors_doc)]

#[cfg(feature = "io")]
pub mod io;
#[cfg(feature = "io")]
pub mod net;
pub mod timers;

#[cfg(feature = "io")]
pub type Driver = timers::Driver<io::Driver>;

#[cfg(not(feature = "io"))]
pub type Driver = timers::Driver<wta_executor::thread::ParkThread>;
