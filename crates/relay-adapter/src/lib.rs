//! Adapter-owned request construction, provider error classification, and optional discovery.
//! Consumer operations unify Relay failures while transport and cancellation remain consumer-owned.

mod adapter;
mod base;
mod conversion;
mod encoders;
mod error;
mod http;
pub mod providers;
mod sse;
mod stream;

pub use adapter::*;
pub use base::{BaseAdapter, Classify};
pub use conversion::*;
pub use encoders::*;
pub use error::*;
pub use http::*;
pub use sse::SseDecoder;
pub use stream::*;
