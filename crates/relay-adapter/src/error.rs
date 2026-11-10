use std::{error::Error, fmt};

use relay_protocol::{ConversionError, ProviderError};

/// Transport and connection failures remain the consumer's errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdapterError {
    Conversion(ConversionError),
    Provider(ProviderError),
}

impl fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conversion(error) => write!(formatter, "conversion error: {error}"),
            Self::Provider(error) => write!(formatter, "provider error: {error}"),
        }
    }
}

impl Error for AdapterError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Conversion(error) => Some(error),
            Self::Provider(error) => Some(error),
        }
    }
}

impl From<ConversionError> for AdapterError {
    fn from(error: ConversionError) -> Self {
        Self::Conversion(error)
    }
}

impl From<ProviderError> for AdapterError {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error)
    }
}
