use relay_bridge::Frame;
use relay_protocol::DecodeError;

/// Incremental SSE framing shared by the built-in adapters and available to custom adapters.
/// `finish` flushes an unterminated final event only if it has a data field,
/// matching the OpenAI SDK. Malformed UTF-8, including in ignored fields, is an error.
#[derive(Default)]
pub struct SseDecoder {
    line: Vec<u8>,
    after_cr: bool,
    event: Option<String>,
    data: Vec<String>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8], out: &mut Vec<Frame>) -> Result<(), DecodeError> {
        for &byte in bytes {
            if std::mem::take(&mut self.after_cr) && byte == b'\n' {
                continue;
            }
            if matches!(byte, b'\r' | b'\n') {
                self.read_line(out)?;
                self.after_cr = byte == b'\r';
            } else {
                self.line.push(byte);
            }
        }
        Ok(())
    }

    pub fn finish(&mut self, out: &mut Vec<Frame>) -> Result<(), DecodeError> {
        if !self.line.is_empty() {
            self.read_line(out)?;
        }
        if !self.data.is_empty() {
            self.dispatch(out);
        } else {
            self.event = None;
        }
        self.after_cr = false;
        Ok(())
    }

    fn read_line(&mut self, out: &mut Vec<Frame>) -> Result<(), DecodeError> {
        let bytes = std::mem::take(&mut self.line);
        let line = std::str::from_utf8(&bytes).map_err(|error| DecodeError {
            path: String::new(),
            message: error.to_string(),
        })?;
        if line.is_empty() {
            self.dispatch(out);
            return Ok(());
        }
        if line.starts_with(':') {
            return Ok(());
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = Some(value.into()),
            "data" => self.data.push(value.into()),
            _ => {}
        }
        Ok(())
    }

    fn dispatch(&mut self, out: &mut Vec<Frame>) {
        let event = self.event.take();
        if event.is_none() && self.data.is_empty() {
            return;
        }
        out.push(Frame {
            event,
            data: std::mem::take(&mut self.data).join("\n"),
        });
    }
}
