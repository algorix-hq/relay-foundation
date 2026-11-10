use std::{error::Error, fmt, sync::Arc};

use relay_bridge::{ApiFormat, Bridge, ConversionOptions, NativeFields, StreamDecoder};
use relay_protocol::{
    ConversionError, ConversionLoss, ProviderError, Request, Response, StreamEvent,
};

use crate::{Adapter, FrameDecoder, FrameItem, Headers, HttpRequest};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncompatibleFormat {
    pub bridge_format: ApiFormat,
    pub supported_formats: Vec<ApiFormat>,
}

impl fmt::Display for IncompatibleFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "bridge format {} is not supported by adapter (",
            self.bridge_format
        )?;
        for (index, format) in self.supported_formats.iter().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            write!(formatter, "{format}")?;
        }
        formatter.write_str(")")
    }
}

impl Error for IncompatibleFormat {}

/// Transport and connection failures remain the consumer's errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PipelineError {
    Conversion(ConversionError),
    Provider(ProviderError),
}

impl fmt::Display for PipelineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conversion(error) => write!(formatter, "conversion error: {error}"),
            Self::Provider(error) => write!(formatter, "provider error: {error}"),
        }
    }
}

impl Error for PipelineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Conversion(error) => Some(error),
            Self::Provider(error) => Some(error),
        }
    }
}

impl From<ConversionError> for PipelineError {
    fn from(error: ConversionError) -> Self {
        Self::Conversion(error)
    }
}

impl From<ProviderError> for PipelineError {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error)
    }
}

/// Build a request, execute it with the consumer's transport, then parse its response.
/// For streaming, call `check_stream_start` before `stream`, supplying the complete
/// error body on a rejected HTTP response (or any initial payload needed by the
/// adapter). On success feed response bytes to `push`, then call `finish` only at
/// normal end of input. On transport failure, abandon the stream without finishing.
pub struct Pipeline {
    bridge: Arc<dyn Bridge>,
    adapter: Arc<dyn Adapter>,
}

impl Pipeline {
    pub fn new(
        bridge: Arc<dyn Bridge>,
        adapter: Arc<dyn Adapter>,
    ) -> Result<Self, IncompatibleFormat> {
        if !adapter.supported_formats().contains(bridge.format()) {
            return Err(IncompatibleFormat {
                bridge_format: bridge.format().clone(),
                supported_formats: adapter.supported_formats().to_vec(),
            });
        }
        Ok(Self { bridge, adapter })
    }

    pub fn build_request(
        &self,
        request: &Request,
        native: &NativeFields,
        stream: bool,
        options: &ConversionOptions,
    ) -> Result<(HttpRequest, Vec<ConversionLoss>), PipelineError> {
        let encoded = self
            .bridge
            .encode_request(request, native, stream, options)?;
        let http = self.adapter.build_request(
            self.bridge.format(),
            &request.model,
            &encoded.body,
            stream,
        )?;
        Ok((http, encoded.losses))
    }

    pub fn parse_response(
        &self,
        status: u16,
        headers: &Headers,
        body: &[u8],
    ) -> Result<(Response, NativeFields), PipelineError> {
        self.adapter.check_response(status, headers, body)?;
        Ok(self.bridge.decode_response(body)?)
    }

    pub fn check_stream_start(
        &self,
        status: u16,
        headers: &Headers,
        body: &[u8],
    ) -> Result<(), PipelineError> {
        Ok(self.adapter.check_response(status, headers, body)?)
    }

    pub fn stream(&self) -> PipelineStream {
        PipelineStream {
            frames: self.adapter.frame_decoder(self.bridge.format()),
            decoder: self.bridge.stream_decoder(),
            terminal: false,
        }
    }
}

/// Events preceding a failure in the same chunk are preserved, never replaced by
/// synthetic `ItemEnd` or `Stop` events.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct StreamChunk {
    pub events: Vec<StreamEvent>,
    pub failure: Option<PipelineError>,
}

/// After failure or `finish`, further calls return no events and no new failure.
pub struct PipelineStream {
    frames: Box<dyn FrameDecoder>,
    decoder: Box<dyn StreamDecoder>,
    terminal: bool,
}

impl PipelineStream {
    pub fn push(&mut self, bytes: &[u8]) -> StreamChunk {
        if self.terminal {
            return StreamChunk::default();
        }
        let frames = self.frames.push(bytes);
        self.decode_frames(frames)
    }

    pub fn finish(&mut self) -> StreamChunk {
        if self.terminal {
            return StreamChunk::default();
        }
        let frames = self.frames.finish();
        let mut chunk = self.decode_frames(frames);
        if chunk.failure.is_none() {
            match self.decoder.finish() {
                Ok(events) => chunk.events.extend(events),
                Err(error) => chunk.failure = Some(error.into()),
            }
        }
        self.terminal = true;
        chunk
    }

    fn decode_frames(&mut self, frames: Result<Vec<FrameItem>, ConversionError>) -> StreamChunk {
        let mut chunk = StreamChunk::default();
        let frames = match frames {
            Ok(frames) => frames,
            Err(error) => {
                self.terminal = true;
                chunk.failure = Some(error.into());
                return chunk;
            }
        };
        for frame in frames {
            let result = match frame {
                FrameItem::Data(frame) => self.decoder.push(frame).map_err(PipelineError::from),
                FrameItem::Error(error) => Err(error.into()),
            };
            match result {
                Ok(events) => chunk.events.extend(events),
                Err(error) => {
                    self.terminal = true;
                    chunk.failure = Some(error);
                    break;
                }
            }
        }
        chunk
    }
}
