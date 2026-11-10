use relay_bridge::StreamDecoder;
use relay_protocol::{ConversionError, StreamEvent};

use crate::{AdapterError, FrameDecoder, FrameItem};

/// Preserves events appended before a failure, without synthetic completion events.
/// After failure or `finish`, calls append nothing and return `Ok(())`.
pub struct AdapterStream {
    pub(crate) frames: Box<dyn FrameDecoder>,
    pub(crate) decoder: Box<dyn StreamDecoder>,
    pub(crate) terminal: bool,
}

impl AdapterStream {
    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<StreamEvent>) -> Result<(), AdapterError> {
        if self.terminal {
            return Ok(());
        }
        let frames = self.frames.push(bytes);
        let result = self.decode_frames(frames, out);
        self.terminal = result.is_err();
        result
    }

    pub fn finish(&mut self, out: &mut Vec<StreamEvent>) -> Result<(), AdapterError> {
        if self.terminal {
            return Ok(());
        }
        self.terminal = true;
        let frames = self.frames.finish();
        self.decode_frames(frames, out)?;
        Ok(self.decoder.finish(out)?)
    }

    fn decode_frames(
        &mut self,
        frames: Result<Vec<FrameItem>, ConversionError>,
        out: &mut Vec<StreamEvent>,
    ) -> Result<(), AdapterError> {
        for frame in frames? {
            match frame {
                FrameItem::Data(frame) => self.decoder.push(frame, out)?,
                FrameItem::Error(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}
