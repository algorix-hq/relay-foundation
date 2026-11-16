use futures_core::future::BoxFuture;
use futures_util::StreamExt;
use relay_adapter::{Headers, Method, ProviderRequest};

use crate::{BoxError, Transport, TransportResponse};

pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Self {
        Self::with_client(reqwest::Client::new())
    }

    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for ReqwestTransport {
    fn send(&self, request: ProviderRequest) -> BoxFuture<'_, Result<TransportResponse, BoxError>> {
        Box::pin(async move {
            let request = build_request(request)?;
            let response = self.client.execute(request).await?;
            let status = response.status().as_u16();
            let headers = response_headers(response.headers())?;
            let body = Box::pin(
                response
                    .bytes_stream()
                    .map(|item| item.map(|bytes| bytes.to_vec()).map_err(BoxError::from)),
            );
            Ok(TransportResponse {
                status,
                headers,
                body,
            })
        })
    }
}

fn build_request(request: ProviderRequest) -> Result<reqwest::Request, BoxError> {
    let method = match request.method {
        Method::Get => reqwest::Method::GET,
        Method::Post => reqwest::Method::POST,
    };
    let mut output = reqwest::Request::new(method, request.url.parse()?);
    for (name, value) in request.headers.0 {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())?;
        let value = reqwest::header::HeaderValue::from_str(&value)?;
        output.headers_mut().append(name, value);
    }
    *output.body_mut() = Some(request.body.into());
    Ok(output)
}

fn response_headers(headers: &reqwest::header::HeaderMap) -> Result<Headers, BoxError> {
    let mut pairs = Vec::new();
    for (name, value) in headers {
        pairs.push((name.to_string(), value.to_str()?.to_owned()));
    }
    Ok(Headers(pairs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_preserves_method_url_duplicate_values_and_body() {
        for (method, expected) in [
            (Method::Get, reqwest::Method::GET),
            (Method::Post, reqwest::Method::POST),
        ] {
            let body = if method == Method::Get {
                Vec::new()
            } else {
                b"{\"model\":\"test\"}".to_vec()
            };
            let request = build_request(ProviderRequest {
                method,
                url: "https://example.com/v1/models?after=first".into(),
                headers: Headers(vec![
                    ("x-first".into(), "one".into()),
                    ("x-second".into(), "two".into()),
                    ("X-First".into(), "three".into()),
                ]),
                body: body.clone(),
            })
            .expect("request");
            assert_eq!(request.method(), expected);
            assert_eq!(
                request.url().as_str(),
                "https://example.com/v1/models?after=first"
            );
            assert_eq!(
                request
                    .headers()
                    .get_all("x-first")
                    .iter()
                    .collect::<Vec<_>>(),
                ["one", "three"]
            );
            assert_eq!(request.headers()["x-second"], "two");
            assert_eq!(request.headers().len(), 3);
            assert_eq!(
                request.body().and_then(reqwest::Body::as_bytes),
                Some(body.as_slice())
            );
        }
    }

    #[test]
    fn request_rejects_invalid_headers() {
        for (name, value) in [("bad name", "value"), ("x-name", "bad\nvalue")] {
            let result = build_request(ProviderRequest {
                method: Method::Post,
                url: "https://example.com".into(),
                headers: Headers(vec![(name.into(), value.into())]),
                body: Vec::new(),
            });
            assert!(result.is_err());
        }
    }
}
