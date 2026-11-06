//! Small error type shared by the modules ported from the Mnemo prototype
//! (`activity`, `ask`). Kept separate from the upstream error handling.

use std::fmt;

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Config(String),
    Protocol(String),
    Index(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::Config(m) => write!(f, "config error: {m}"),
            Error::Protocol(m) => write!(f, "protocol error: {m}"),
            Error::Index(m) => write!(f, "index error: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
