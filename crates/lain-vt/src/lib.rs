//! lain-vt: terminal emulation core. No GUI, no I/O — bytes in, grid out.
//!
//! - [`parser`]: VT500 state machine (CSI/OSC/DCS/APC, UTF-8).
//! - [`term`]: screen state, protocols and replies.
//! - [`input`]: keyboard encoding (legacy xterm + kitty protocol).
//! - [`mouse`]: mouse reporting (X10, UTF-8, SGR, urxvt, SGR-pixels).
//! - [`selection`]: text selection and extraction.

pub mod cell;
pub mod color;
pub mod graphics;
pub mod input;
pub mod mouse;
pub mod parser;
pub mod search;
pub mod selection;
pub mod term;
pub mod util;

pub use cell::{Attrs, Cell, Color, Mark, Row, Underline, flags};
pub use color::{Rgb, Theme};
pub use term::{CursorShape, Event, Hyperlink, Modes, MouseEncoding, MouseTracking, Term};
