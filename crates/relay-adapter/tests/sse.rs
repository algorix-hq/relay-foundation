use relay_adapter::{Adapter, AdapterExt, Headers, OpenAiCompatibleAdapter, SseDecoder};
use relay_bridge::Frame;
use relay_protocol::{RelayError, StreamEvent};

fn decode(chunks: &[&[u8]]) -> Vec<Frame> {
    let mut decoder = SseDecoder::new();
    let mut frames = Vec::new();
    for chunk in chunks {
        decoder.push(chunk, &mut frames).expect("framing");
    }
    decoder.finish(&mut frames).expect("EOF");
    frames
}

#[test]
fn every_byte_boundary_in_recorded_stream_has_identical_frames_including_split_crlf() {
    let recorded = include_str!("fixtures/live/ollama_stream_usage.sse");
    let expected: Vec<_> = recorded
        .split("\n\n")
        .filter(|record| !record.is_empty())
        .map(|record| Frame {
            event: None,
            data: record.strip_prefix("data: ").expect("recorded data").into(),
        })
        .collect();
    for ending in ["\n", "\r", "\r\n"] {
        let wire = recorded.replace('\n', ending);
        for split in 0..=wire.len() {
            assert_eq!(
                decode(&[&wire.as_bytes()[..split], &wire.as_bytes()[split..]]),
                expected,
                "{ending:?} byte {split}"
            );
        }
        assert_eq!(
            decode(&wire.as_bytes().chunks(1).collect::<Vec<_>>()),
            expected
        );
    }
}

#[test]
fn comments_multiline_data_single_space_stripping_and_unterminated_final_event_follow_sdk_rules() {
    let wire = ":keepalive\r\n\r\nevent: old\nevent: update\rdata: 한\r\n:comment\ndata:  two\ndata\nignored: ignored\r\n\r\nevent: final\ndata: tail";
    let expected = vec![
        Frame {
            event: Some("update".into()),
            data: "한\n two\n".into(),
        },
        Frame {
            event: Some("final".into()),
            data: "tail".into(),
        },
    ];
    assert_eq!(
        decode(&wire.as_bytes().chunks(1).collect::<Vec<_>>()),
        expected
    );
    assert!(decode(&[b"event: no-data"]).is_empty());
    assert_eq!(
        decode(&[b"event: dispatched\n\n"]),
        vec![Frame {
            event: Some("dispatched".into()),
            data: String::new()
        }]
    );
    assert_eq!(
        decode(&[b"data:"]),
        vec![Frame {
            event: None,
            data: String::new()
        }]
    );
}

#[test]
fn compatible_recorded_stream_completes_with_final_usage_and_eof_flushes_error_data() {
    let adapter = OpenAiCompatibleAdapter::new("https://ollama.com/v1/chat/completions");
    let mut stream = adapter
        .open_stream(200, &Headers::default())
        .expect("stream");
    let mut events = Vec::new();
    for chunk in include_bytes!("fixtures/live/ollama_stream_usage.sse").chunks(1) {
        events.extend(stream.push(chunk));
    }
    events.extend(stream.finish());
    assert!(events.iter().all(Result::is_ok));
    assert!(matches!(
        events.last(),
        Some(Ok(StreamEvent::Stop {
            usage: Some(relay_protocol::Usage {
                input_tokens: 19,
                output_tokens: 4,
                ..
            }),
            ..
        }))
    ));

    let mut stream = adapter
        .open_stream(200, &Headers::default())
        .expect("stream");
    assert!(
        stream
            .push(br#"data: {"error":{"code":"insufficient_quota","message":"empty balance"}}"#)
            .next()
            .is_none()
    );
    let items: Vec<_> = stream.finish().collect();
    assert_eq!(items.len(), 1);
    assert!(
        matches!(&items[0], Err(RelayError::Provider(error)) if error.category == relay_protocol::ProviderErrorCategory::InsufficientQuota && error.http_status.is_none())
    );
}

#[test]
fn malformed_utf8_preserves_prior_frames_and_becomes_decode_after_earlier_stream_events() {
    for suffix in [b"data: \xff\n\n".as_slice(), b":\xff\n", b"ignored:\xff"] {
        let mut decoder = SseDecoder::new();
        let mut frames = Vec::new();
        let result = decoder
            .push(
                &[b"data: good\n\n".as_slice(), suffix].concat(),
                &mut frames,
            )
            .and_then(|()| decoder.finish(&mut frames));
        assert!(result.is_err());
        assert_eq!(
            frames,
            vec![Frame {
                event: None,
                data: "good".into()
            }]
        );
    }
    let adapter = OpenAiCompatibleAdapter::new("https://ollama.com/v1/chat/completions");
    let mut stream = adapter
        .open_stream(200, &Headers::default())
        .expect("stream");
    let prefix = include_str!("fixtures/live/ollama_stream_usage.sse")
        .split("\n\n")
        .next()
        .expect("first frame");
    let mut wire = format!("{prefix}\n\n").into_bytes();
    wire.extend_from_slice(b"data: \xff\n\n");
    let items = stream.push(&wire).collect::<Vec<_>>();
    assert!(matches!(items[0], Ok(StreamEvent::Start { .. })));
    assert!(matches!(items.last(), Some(Err(RelayError::Decode(_)))));
    assert_eq!(items.iter().filter(|item| item.is_err()).count(), 1);
    let mut frames = Vec::new();
    let mut decoder = adapter.frame_decoder(&Headers::default());
    decoder
        .push(b"data: \xe2", &mut frames)
        .expect("incomplete chunk");
    assert!(decoder.finish(&mut frames).is_err());
}
