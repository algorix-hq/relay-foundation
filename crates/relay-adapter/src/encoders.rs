use relay_bridge::Frame;
use relay_protocol::{ProviderErrorCategory as Category, RelayError, RetryAfter};
use serde_json::{Value, json};

use crate::{EncodedError, ErrorEncoder, Headers, Stage};

#[derive(Clone, Copy, Debug, Default)]
pub struct ChatCompletionsErrors;

/// Standalone stream errors use sequence number zero. The normal Responses
/// stream encoder owns numbering; the ErrorEncoder contract cannot observe its
/// counter, so consumers needing continuous numbering must rewrite this field.
#[derive(Clone, Copy, Debug, Default)]
pub struct ResponsesErrors;

/// Error envelopes include the SDK-required `request_id: null`. Upstream IDs
/// are diagnostics, not IDs issued by the client-facing Messages endpoint.
#[derive(Clone, Copy, Debug, Default)]
pub struct MessagesErrors;

#[derive(Clone, Copy)]
enum Format {
    ChatCompletions,
    Responses,
    Messages,
}

macro_rules! encoder {
    ($name:ident, $format:ident) => {
        impl $name {
            pub const fn new() -> Self {
                Self
            }
        }

        impl ErrorEncoder for $name {
            fn encode_error(&self, error: &RelayError, stage: Stage) -> EncodedError {
                Format::$format.encode(error, stage)
            }

            fn encode_stream_error(&self, error: &RelayError) -> Vec<Frame> {
                vec![Format::$format.stream(error)]
            }
        }
    };
}

encoder!(ChatCompletionsErrors, ChatCompletions);
encoder!(ResponsesErrors, Responses);
encoder!(MessagesErrors, Messages);

impl Format {
    fn encode(self, error: &RelayError, stage: Stage) -> EncodedError {
        let mut headers = Headers(vec![("content-type".into(), "application/json".into())]);
        if let RelayError::Provider(provider) = error {
            if let Some(retry_after) = &provider.retry_after {
                let value = match retry_after {
                    RetryAfter::Seconds { seconds } => seconds.to_string(),
                    RetryAfter::HttpDate { date } => date.clone(),
                };
                headers.0.push(("retry-after".into(), value));
            }
            if let Some(request_id) = &provider.request_id {
                headers
                    .0
                    .push(("x-relay-upstream-request-id".into(), request_id.clone()));
            }
        }
        EncodedError {
            status: self.status(error, stage),
            headers,
            body: self.body(error, stage).to_string().into_bytes(),
        }
    }

    fn status(self, error: &RelayError, stage: Stage) -> u16 {
        match error {
            RelayError::Provider(provider) => match provider.category {
                Category::Authentication => 401,
                Category::PermissionDenied => 403,
                Category::InsufficientQuota if matches!(self, Self::Messages) => 402,
                Category::InsufficientQuota | Category::RateLimited => 429,
                Category::Overloaded if matches!(self, Self::Messages) => 529,
                Category::Overloaded => 503,
                Category::InvalidRequest
                | Category::UnsupportedFeature
                | Category::ContextLengthExceeded => 400,
                Category::ModelNotFound => 404,
                Category::ServerError => 500,
                Category::Unknown => provider.http_status.unwrap_or(502),
            },
            RelayError::Conversion(_) if stage == Stage::Request => 400,
            RelayError::Conversion(_) | RelayError::Decode(_) | RelayError::IncompleteStream(_) => {
                502
            }
            RelayError::Configuration(_) => 500,
            RelayError::UnsupportedOperation(_) => 501,
        }
    }

    fn body(self, error: &RelayError, stage: Stage) -> Value {
        let category = match error {
            RelayError::Provider(provider) => provider.category,
            RelayError::Conversion(_) if stage == Stage::Request => Category::InvalidRequest,
            _ => Category::ServerError,
        };
        let message = match error {
            RelayError::Provider(provider) => provider.message.clone(),
            _ => error.to_string(),
        };
        if matches!(self, Self::Messages) {
            let kind = match category {
                Category::Authentication => "authentication_error",
                Category::PermissionDenied => "permission_error",
                Category::InsufficientQuota => "billing_error",
                Category::RateLimited => "rate_limit_error",
                Category::Overloaded => "overloaded_error",
                Category::InvalidRequest
                | Category::UnsupportedFeature
                | Category::ContextLengthExceeded => "invalid_request_error",
                Category::ModelNotFound => "not_found_error",
                Category::ServerError | Category::Unknown => "api_error",
            };
            return json!({"type":"error", "error":{"type":kind, "message":message}, "request_id":null});
        }
        let (kind, code) = match category {
            Category::Authentication => ("invalid_request_error", Some("invalid_api_key")),
            Category::PermissionDenied
            | Category::InvalidRequest
            | Category::UnsupportedFeature => ("invalid_request_error", None),
            Category::InsufficientQuota => ("insufficient_quota", Some("insufficient_quota")),
            Category::RateLimited => ("rate_limit_error", Some("rate_limit_exceeded")),
            Category::Overloaded => ("service_unavailable_error", Some("server_is_overloaded")),
            Category::ModelNotFound => ("invalid_request_error", Some("model_not_found")),
            Category::ContextLengthExceeded => {
                ("invalid_request_error", Some("context_length_exceeded"))
            }
            Category::ServerError => ("server_error", Some("server_error")),
            Category::Unknown => ("api_error", None),
        };
        json!({"error":{"message":message, "type":kind, "code":code, "param":null}})
    }

    fn stream(self, error: &RelayError) -> Frame {
        let mut body = self.body(error, Stage::Response);
        let event = match self {
            Self::ChatCompletions => None,
            Self::Messages => Some("error".into()),
            Self::Responses => {
                let object = body["error"].take();
                body = json!({"type":"error", "code":object["code"], "message":object["message"], "param":null, "sequence_number":0});
                Some("error".into())
            }
        };
        Frame {
            event,
            data: body.to_string(),
        }
    }
}
