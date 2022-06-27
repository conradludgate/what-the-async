#![forbid(unsafe_code)]
#![warn(clippy::pedantic)]
#![allow(clippy::missing_errors_doc)]

pub mod io;
pub mod net;
pub mod timers;

pub type Driver = timers::Driver<io::Driver>;
