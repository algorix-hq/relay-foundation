use std::sync::Arc;

use relay_bridge::{ApiFormat, Bridge, ConversionOptions};
use relay_protocol::ConfigurationError;

pub struct Conversion {
    bridge: Arc<dyn Bridge>,
    options: ConversionOptions,
}

impl Conversion {
    pub fn new(
        bridge: Arc<dyn Bridge>,
        supported_formats: &[ApiFormat],
        options: ConversionOptions,
    ) -> Result<Self, ConfigurationError> {
        if !supported_formats.contains(bridge.format()) {
            let supported = supported_formats
                .iter()
                .map(ApiFormat::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            return Err(ConfigurationError {
                message: format!(
                    "bridge format {} is not supported by adapter (supported formats: [{supported}])",
                    bridge.format()
                ),
            });
        }
        Ok(Self { bridge, options })
    }

    pub fn bridge(&self) -> &dyn Bridge {
        self.bridge.as_ref()
    }

    pub fn options(&self) -> &ConversionOptions {
        &self.options
    }
}
