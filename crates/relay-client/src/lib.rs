//! Generation, streaming, and model discovery through an adapter and an injected transport.
//! Default builds have no HTTP client or async runtime dependency.
//! Requests have no retry, timeout, or fallback; dropping a future or stream cancels the request.
//! The opt-in `reqwest` feature provides `ReqwestTransport`.

use std::{
    collections::VecDeque,
    fmt,
    future::poll_fn,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, ready},
};

use futures_core::{Stream, future::BoxFuture, stream::BoxStream};
use relay_adapter::{Adapter, AdapterExt, AdapterStream, Headers, ModelPage, ProviderRequest};
use relay_protocol::{ConversionLoss, Delivery, Error, Request, Response, StreamEvent};

#[cfg(feature = "reqwest")]
mod reqwest;
#[cfg(feature = "reqwest")]
pub use reqwest::ReqwestTransport;

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

pub struct TransportResponse {
    pub status: u16,
    pub headers: Headers,
    pub body: BoxStream<'static, Result<Vec<u8>, BoxError>>,
}

pub trait Transport: Send + Sync {
    fn send(&self, request: ProviderRequest) -> BoxFuture<'_, Result<TransportResponse, BoxError>>;
}

#[derive(Debug)]
pub enum ClientError {
    Protocol(Error),
    Transport(BoxError),
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Protocol(error) => error.fmt(formatter),
            Self::Transport(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(match self {
            Self::Protocol(error) => error,
            Self::Transport(error) => error.as_ref(),
        })
    }
}

impl From<Error> for ClientError {
    fn from(error: Error) -> Self {
        Self::Protocol(error)
    }
}

#[derive(Clone)]
pub struct Client {
    adapter: Arc<dyn Adapter>,
    transport: Arc<dyn Transport>,
}

impl Client {
    pub fn new(adapter: Arc<dyn Adapter>, transport: Arc<dyn Transport>) -> Self {
        Self { adapter, transport }
    }

    pub async fn generate(&self, request: &Request) -> Result<Generated, ClientError> {
        let mut request = request.clone();
        request.delivery = None;
        let (request, losses) = self.adapter.build_request(&request)?;
        let response = self
            .transport
            .send(request)
            .await
            .map_err(ClientError::Transport)?;
        let body = read_body(response.body).await?;
        let response = self
            .adapter
            .parse_response(response.status, &response.headers, &body)?;
        Ok(Generated { response, losses })
    }

    /// Forces streaming, preserving `stream_usage` when delivery is present and defaulting it to true otherwise.
    pub async fn stream(&self, request: &Request) -> Result<EventStream, ClientError> {
        let mut request = request.clone();
        request.delivery = Some(Delivery {
            stream: true,
            stream_usage: request
                .delivery
                .is_none_or(|delivery| delivery.stream_usage),
        });
        let (request, losses) = self.adapter.build_request(&request)?;
        let response = self
            .transport
            .send(request)
            .await
            .map_err(ClientError::Transport)?;
        let adapter_stream = match self.adapter.open_stream(response.status, &response.headers) {
            Ok(stream) => stream,
            Err(rejected) => {
                let body = read_body(response.body).await?;
                return Err(rejected.classify(&body).into());
            }
        };
        Ok(EventStream {
            body: response.body,
            adapter_stream,
            pending: VecDeque::new(),
            losses,
            done: false,
        })
    }

    pub async fn list_models(&self, cursor: Option<&str>) -> Result<ModelPage, ClientError> {
        let request = self.adapter.list_models(cursor)?;
        let response = self
            .transport
            .send(request)
            .await
            .map_err(ClientError::Transport)?;
        let body = read_body(response.body).await?;
        Ok(self
            .adapter
            .parse_models(response.status, &response.headers, &body)?)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Generated {
    pub response: Response,
    pub losses: Vec<ConversionLoss>,
}

pub struct EventStream {
    body: BoxStream<'static, Result<Vec<u8>, BoxError>>,
    adapter_stream: AdapterStream,
    pending: VecDeque<Result<StreamEvent, ClientError>>,
    losses: Vec<ConversionLoss>,
    done: bool,
}

impl EventStream {
    pub fn losses(&self) -> &[ConversionLoss] {
        &self.losses
    }
}

impl Stream for EventStream {
    type Item = Result<StreamEvent, ClientError>;

    fn poll_next(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let stream = self.get_mut();
        loop {
            if let Some(item) = stream.pending.pop_front() {
                if item.is_err() {
                    stream.done = true;
                    stream.pending.clear();
                }
                return Poll::Ready(Some(item));
            }
            if stream.done {
                return Poll::Ready(None);
            }
            match ready!(stream.body.as_mut().poll_next(context)) {
                Some(Ok(bytes)) => stream.pending.extend(
                    stream
                        .adapter_stream
                        .push(&bytes)
                        .map(|item| item.map_err(ClientError::Protocol)),
                ),
                Some(Err(error)) => {
                    stream.pending.push_back(Err(ClientError::Transport(error)));
                    stream.done = true;
                }
                None => {
                    stream.pending.extend(
                        stream
                            .adapter_stream
                            .finish()
                            .map(|item| item.map_err(ClientError::Protocol)),
                    );
                    stream.done = true;
                }
            }
        }
    }
}

async fn read_body(
    mut body: BoxStream<'static, Result<Vec<u8>, BoxError>>,
) -> Result<Vec<u8>, ClientError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = poll_fn(|context| body.as_mut().poll_next(context)).await {
        bytes.extend(chunk.map_err(ClientError::Transport)?);
    }
    Ok(bytes)
}
