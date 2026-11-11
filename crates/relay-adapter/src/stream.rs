use std::{sync::Arc, vec};

use relay_bridge::{Bridge, ConversionOptions, Frame, StreamDecoder, StreamEncoder};
use relay_protocol::{ConversionLoss, DecodeError, RelayError, Request, StreamEvent};

use crate::{ErrorEncoder, FrameDecoder, FrameItem};

/// Preserves events before failure, with the failure last and no synthetic completion.
/// After failure or `finish`, both calls yield nothing. On transport failure or
/// cancellation, drop the stream without calling `finish`.
pub struct AdapterStream {
    pub(crate) frames: Box<dyn FrameDecoder>,
    pub(crate) decoder: Box<dyn StreamDecoder>,
    pub(crate) terminal: bool,
}

pub struct StreamItems(vec::IntoIter<Result<StreamEvent, RelayError>>);

impl Iterator for StreamItems {
    type Item = Result<StreamEvent, RelayError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
}

impl AdapterStream {
    pub fn push(&mut self, bytes: &[u8]) -> StreamItems {
        if self.terminal {
            return StreamItems(Vec::new().into_iter());
        }
        let mut frames = Vec::new();
        let framing = self.frames.push(bytes, &mut frames);
        let mut events = Vec::new();
        let result = self.decode_frames(frames, framing, &mut events);
        self.items(events, result)
    }

    pub fn finish(&mut self) -> StreamItems {
        if self.terminal {
            return StreamItems(Vec::new().into_iter());
        }
        self.terminal = true;
        let mut frames = Vec::new();
        let framing = self.frames.finish(&mut frames);
        let mut events = Vec::new();
        let result = self
            .decode_frames(frames, framing, &mut events)
            .and_then(|()| self.decoder.finish(&mut events).map_err(RelayError::from));
        self.items(events, result)
    }

    pub fn transcode(
        self,
        client: &dyn Bridge,
        request: &Request,
        options: &ConversionOptions,
        errors: Arc<dyn ErrorEncoder>,
    ) -> Transcoder {
        Transcoder {
            stream: self,
            encoder: client.stream_encoder(request, options),
            errors,
            report: TranscodeReport {
                losses: Vec::new(),
                outcome: None,
            },
        }
    }

    fn decode_frames(
        &mut self,
        frames: Vec<FrameItem>,
        framing: Result<(), DecodeError>,
        events: &mut Vec<StreamEvent>,
    ) -> Result<(), RelayError> {
        for frame in frames {
            match frame {
                FrameItem::Data(frame) => self.decoder.push(frame, events)?,
                FrameItem::Error(error) => return Err(error.into()),
            }
        }
        Ok(framing?)
    }

    fn items(&mut self, events: Vec<StreamEvent>, result: Result<(), RelayError>) -> StreamItems {
        let mut items: Vec<_> = events.into_iter().map(Ok).collect();
        if let Err(error) = result {
            self.terminal = true;
            items.push(Err(error));
        }
        StreamItems(items.into_iter())
    }
}

/// Provider bytes in, client-format frames out. Consumers needing retry or fallback
/// before anything reaches the client should use `AdapterStream` directly instead.
/// On transport failure or cancellation, drop this helper without calling `finish`.
pub struct Transcoder {
    stream: AdapterStream,
    encoder: Box<dyn StreamEncoder>,
    errors: Arc<dyn ErrorEncoder>,
    report: TranscodeReport,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscodeReport {
    /// Losses observed so far, including before failure or abandonment.
    pub losses: Vec<ConversionLoss>,
    /// `None` while running or abandoned; transport cuts and cancellation are not
    /// incomplete-stream errors unless the consumer explicitly calls `finish`.
    pub outcome: Option<Outcome>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Failed(RelayError),
}

impl Transcoder {
    pub fn push(&mut self, bytes: &[u8]) -> Vec<Frame> {
        if self.report.outcome.is_some() {
            return Vec::new();
        }
        let items = self.stream.push(bytes);
        self.encode(items)
    }

    pub fn finish(&mut self) -> Vec<Frame> {
        if self.report.outcome.is_some() {
            return Vec::new();
        }
        let items = self.stream.finish();
        let mut frames = self.encode(items);
        if self.report.outcome.is_some() {
            return frames;
        }
        match self.encoder.finish(&mut frames, &mut self.report.losses) {
            Ok(()) => {
                self.report.outcome = Some(Outcome::Completed);
            }
            Err(error) => self.fail(error.into(), &mut frames),
        }
        frames
    }

    pub fn report(&self) -> &TranscodeReport {
        &self.report
    }

    fn encode(&mut self, items: StreamItems) -> Vec<Frame> {
        let mut frames = Vec::new();
        for item in items {
            let result = item.and_then(|event| {
                self.encoder
                    .push(&event, &mut frames, &mut self.report.losses)
                    .map_err(RelayError::from)
            });
            if let Err(error) = result {
                self.fail(error, &mut frames);
                break;
            }
        }
        frames
    }

    fn fail(&mut self, error: RelayError, frames: &mut Vec<Frame>) {
        frames.extend(self.errors.encode_stream_error(&error));
        self.report.outcome = Some(Outcome::Failed(error));
    }
}
