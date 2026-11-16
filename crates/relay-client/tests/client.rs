use std::{
    collections::VecDeque,
    fs,
    future::poll_fn,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use futures_core::{Stream, future::BoxFuture};
use futures_executor::block_on;
use relay_adapter::{
    AdapterExt, Headers, Method, ModelPage, ProviderRequest,
    providers::{anthropic::AnthropicAdapter, openai_compatible::OpenAiCompatibleAdapter},
};
use relay_client::{BoxError, Client, ClientError, EventStream, Transport, TransportResponse};
use relay_protocol::{
    Delivery, Error, Message, Request, StreamEvent, TextPart, UserContent, Version,
};
use serde_json::{Value, json};

const DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../relay-adapter/tests/fixtures/e2e/"
);

struct BodyChunks {
    chunks: VecDeque<Result<Vec<u8>, BoxError>>,
    polls: Arc<AtomicUsize>,
}

impl Stream for BodyChunks {
    type Item = Result<Vec<u8>, BoxError>;

    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        Poll::Ready(self.chunks.pop_front())
    }
}

struct FakeTransport {
    response: Mutex<Option<TransportResponse>>,
    requests: Mutex<Vec<ProviderRequest>>,
}

impl FakeTransport {
    fn new(response: TransportResponse) -> Self {
        Self {
            response: Mutex::new(Some(response)),
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl Transport for FakeTransport {
    fn send(&self, request: ProviderRequest) -> BoxFuture<'_, Result<TransportResponse, BoxError>> {
        self.requests.lock().expect("requests").push(request);
        let response = self.response.lock().expect("response").take();
        Box::pin(async move { Ok(response.expect("one request")) })
    }
}

fn adapter() -> Arc<AnthropicAdapter> {
    Arc::new(
        AnthropicAdapter::new("sk-test")
            .default_max_tokens(16)
            .with_clock(|| 1235)
            .with_id_generator(|| "response-fallback".into()),
    )
}

fn request() -> Request {
    Request {
        version: Version::V1,
        model: "claude-haiku-5-5".into(),
        messages: vec![Message::User {
            content: vec![UserContent::Text(TextPart {
                text: "Say hi in two words.".into(),
                cache_control: None,
            })],
        }],
        tools: None,
        parameters: None,
        response_format: None,
        reasoning: None,
        delivery: None,
    }
}

fn recorded(name: &str, extension: &str) -> (u16, Headers, Vec<u8>) {
    let text = fs::read_to_string(format!("{DIR}{name}.response.headers")).expect("headers");
    let mut lines = text.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("status line");
    let pairs = lines
        .filter_map(|line| line.split_once(": "))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    let body = fs::read(format!("{DIR}{name}.response.{extension}")).expect("body");
    (status, Headers(pairs), body)
}

fn response(status: u16, headers: Headers, bytes: &[u8]) -> TransportResponse {
    TransportResponse {
        status,
        headers,
        body: Box::pin(BodyChunks {
            chunks: bytes.chunks(7).map(|chunk| Ok(chunk.to_vec())).collect(),
            polls: Arc::new(AtomicUsize::new(0)),
        }),
    }
}

async fn next(stream: &mut EventStream) -> Option<Result<StreamEvent, ClientError>> {
    poll_fn(|context| Pin::new(&mut *stream).poll_next(context)).await
}

#[test]
fn generate_forces_non_streaming_and_returns_the_adapter_response() {
    block_on(async {
        let adapter = adapter();
        let (status, headers, bytes) = recorded("anthropic_plain", "json");
        let expected = adapter
            .parse_response(status, &headers, &bytes)
            .expect("parse");
        let transport = Arc::new(FakeTransport::new(response(status, headers, &bytes)));
        let client = Client::new(adapter, transport.clone());
        let mut request = request();
        request.delivery = Some(Delivery {
            stream: true,
            stream_usage: true,
        });
        let generated = client.generate(&request).await.expect("generate");
        assert_eq!(generated.response, expected);
        assert!(generated.losses.is_empty());
        let requests = transport.requests.lock().expect("requests");
        assert_eq!(requests.len(), 1);
        let sent: Value = serde_json::from_slice(&requests[0].body).expect("JSON");
        assert_ne!(sent["stream"], true);
        assert!(request.delivery.expect("unchanged delivery").stream);
    });
}

#[test]
fn streaming_chunks_match_direct_adapter_events_and_end() {
    block_on(async {
        let adapter = Arc::new(
            OpenAiCompatibleAdapter::new("https://ollama.com/v1/chat/completions")
                .api_key("sk-test")
                .without_tool_choice()
                .with_clock(|| 1235)
                .with_id_generator(|| "response-fallback".into()),
        );
        let (status, headers, bytes) = recorded("ollama_stream", "sse");
        let mut direct = adapter.open_stream(status, &headers).expect("open");
        let mut expected: Vec<_> = direct
            .push(&bytes)
            .map(|item| item.expect("event"))
            .collect();
        expected.extend(direct.finish().map(|item| item.expect("finish")));
        for (delivery, stream_usage) in [
            (None, true),
            (
                Some(Delivery {
                    stream: false,
                    stream_usage: false,
                }),
                false,
            ),
        ] {
            let transport = Arc::new(FakeTransport::new(response(
                status,
                headers.clone(),
                &bytes,
            )));
            let client = Client::new(adapter.clone(), transport.clone());
            let mut request = request();
            request.model = "gemma4:31b".into();
            request.delivery = delivery;
            let mut stream = client.stream(&request).await.expect("stream");
            assert!(stream.losses().is_empty());
            let mut events = Vec::new();
            while let Some(item) = next(&mut stream).await {
                events.push(item.expect("event"));
            }
            assert_eq!(events, expected);
            assert!(next(&mut stream).await.is_none());
            let requests = transport.requests.lock().expect("requests");
            assert_eq!(requests.len(), 1);
            let sent: Value = serde_json::from_slice(&requests[0].body).expect("JSON");
            assert_eq!(sent["stream"], true);
            assert_eq!(
                sent["stream_options"]["include_usage"]
                    .as_bool()
                    .unwrap_or(false),
                stream_usage
            );
            assert_eq!(request.delivery, delivery);
        }
    });
}

#[test]
fn rejected_stream_returns_a_provider_error_before_events() {
    block_on(async {
        let bytes =
            br#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#;
        let transport = Arc::new(FakeTransport::new(response(429, Headers::default(), bytes)));
        let client = Client::new(adapter(), transport);
        assert!(matches!(
            client.stream(&request()).await,
            Err(ClientError::Protocol(Error::Provider(error))) if error.http_status == Some(429)
        ));
    });
}

#[test]
fn transport_failure_preserves_earlier_events_and_stops_polling() {
    block_on(async {
        let adapter = adapter();
        let (status, headers, bytes) = recorded("anthropic_stream", "sse");
        let end = bytes
            .windows(b"event: content_block_stop".len())
            .position(|window| window == b"event: content_block_stop")
            .expect("partial stream boundary");
        let bytes = &bytes[..end];
        let mut direct = adapter.open_stream(status, &headers).expect("open");
        let expected: Vec<_> = direct
            .push(bytes)
            .map(|item| item.expect("event"))
            .collect();
        assert!(!expected.is_empty());
        let polls = Arc::new(AtomicUsize::new(0));
        let mut chunks: VecDeque<_> = bytes.chunks(7).map(|chunk| Ok(chunk.to_vec())).collect();
        chunks.push_back(Err(std::io::Error::other("connection lost").into()));
        chunks.push_back(Ok(b"must not be read".to_vec()));
        let transport = Arc::new(FakeTransport::new(TransportResponse {
            status,
            headers,
            body: Box::pin(BodyChunks {
                chunks,
                polls: polls.clone(),
            }),
        }));
        let client = Client::new(adapter, transport);
        let mut stream = client.stream(&request()).await.expect("stream");
        let mut events = Vec::new();
        loop {
            match next(&mut stream).await {
                Some(Ok(event)) => events.push(event),
                Some(Err(ClientError::Transport(_))) => break,
                other => panic!("expected transport failure, got {other:?}"),
            }
        }
        assert_eq!(events, expected);
        let polls_at_failure = polls.load(Ordering::SeqCst);
        assert!(next(&mut stream).await.is_none());
        assert!(next(&mut stream).await.is_none());
        assert_eq!(polls.load(Ordering::SeqCst), polls_at_failure);
    });
}

#[test]
fn model_discovery_passes_the_cursor_and_returns_the_page() {
    block_on(async {
        let bytes = serde_json::to_vec(&json!({
            "data": [{"id": "model-one"}, {"id": "model-two"}],
            "has_more": true,
            "last_id": "model-two"
        }))
        .expect("page");
        let transport = Arc::new(FakeTransport::new(response(
            200,
            Headers::default(),
            &bytes,
        )));
        let client = Client::new(adapter(), transport.clone());
        assert_eq!(
            client.list_models(Some("previous")).await.expect("models"),
            ModelPage {
                model_ids: vec!["model-one".into(), "model-two".into()],
                next_cursor: Some("model-two".into()),
            }
        );
        let requests = transport.requests.lock().expect("requests");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, Method::Get);
        assert!(requests[0].url.ends_with("&after_id=previous"));
        assert!(requests[0].body.is_empty());
    });
}

#[test]
fn client_futures_are_send_and_event_stream_is_send_and_unpin() {
    fn assert_send<T: Send>(_: &T) {}
    fn assert_stream<T: Send + Unpin>() {}

    let transport = Arc::new(FakeTransport::new(response(200, Headers::default(), b"")));
    let client = Client::new(adapter(), transport);
    let request = request();
    assert_send(&client.generate(&request));
    assert_send(&client.stream(&request));
    assert_send(&client.list_models(None));
    assert_stream::<EventStream>();
}
