use std::{error::Error, fmt};

use relay_protocol::{ConversionError, DecodeError, IncompleteStream};

/// There is no provider variant by design: only adapters classify provider failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BridgeError {
    Conversion(ConversionError),
    Decode(DecodeError),
    IncompleteStream(IncompleteStream),
}

impl fmt::Display for BridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conversion(error) => write!(formatter, "conversion error: {error}"),
            Self::Decode(error) => write!(formatter, "decode error: {error}"),
            Self::IncompleteStream(error) => write!(formatter, "incomplete stream: {error}"),
        }
    }
}

impl Error for BridgeError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Conversion(error) => Some(error),
            Self::Decode(error) => Some(error),
            Self::IncompleteStream(error) => Some(error),
        }
    }
}

impl From<ConversionError> for BridgeError {
    fn from(error: ConversionError) -> Self {
        Self::Conversion(error)
    }
}

impl From<DecodeError> for BridgeError {
    fn from(error: DecodeError) -> Self {
        Self::Decode(error)
    }
}

impl From<IncompleteStream> for BridgeError {
    fn from(error: IncompleteStream) -> Self {
        Self::IncompleteStream(error)
    }
}

impl From<BridgeError> for relay_protocol::Error {
    fn from(error: BridgeError) -> Self {
        match error {
            BridgeError::Conversion(error) => Self::Conversion(error),
            BridgeError::Decode(error) => Self::Decode(error),
            BridgeError::IncompleteStream(error) => Self::IncompleteStream(error),
        }
    }
}
