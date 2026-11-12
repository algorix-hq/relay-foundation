//! Adapter-owned request construction, provider error classification, and optional discovery.
//! Consumer operations unify Relay failures while transport and cancellation remain consumer-owned.

mod adapter;
mod builtin;
mod conversion;
mod encoders;
mod error;
mod http;
mod sse;
mod stream;

pub use adapter::*;
pub use builtin::{AnthropicAdapter, OpenAiAdapter, OpenAiCompatibleAdapter};
pub use conversion::*;
pub use encoders::*;
pub use error::*;
pub use http::*;
pub use sse::SseDecoder;
pub use stream::*;
