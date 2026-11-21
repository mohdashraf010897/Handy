//! Platform-independent audio, adapter, and session contracts for lowyap.
//!
//! The domain describes audio and transcripts, the ports let callers substitute
//! adapters, and the session bus produces the shared UI wire snapshot without
//! owning a clock, runtime, or event transport.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod domain;
pub mod ports;
pub mod session_bus;

pub use domain::*;
pub use ports::*;
pub use session_bus::*;

#[cfg(test)]
mod test_support {
    use std::fmt::Debug;

    use serde::{de::DeserializeOwned, Serialize};
    use serde_json::Value;

    pub(crate) fn assert_wire<T>(value: T, expected: Value)
    where
        T: Debug + PartialEq + Serialize + DeserializeOwned,
    {
        assert_eq!(serde_json::to_value(&value).unwrap(), expected);
        assert_eq!(serde_json::from_value::<T>(expected).unwrap(), value);
    }
}
