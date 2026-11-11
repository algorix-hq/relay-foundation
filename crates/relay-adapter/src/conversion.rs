use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use relay_bridge::{ApiFormat, Bridge, ConversionOptions, DecodeContext};
use relay_protocol::ConfigurationError;

pub struct Conversion {
    bridge: Arc<dyn Bridge>,
    options: ConversionOptions,
    id_generator: Arc<dyn Fn() -> String + Send + Sync>,
    clock: Arc<dyn Fn() -> u64 + Send + Sync>,
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
        Ok(Self::from_bridge(bridge, options))
    }

    pub(crate) fn from_bridge(bridge: Arc<dyn Bridge>, options: ConversionOptions) -> Self {
        Self {
            bridge,
            options,
            id_generator: Arc::new(|| uuid::Uuid::new_v4().to_string()),
            clock: Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs()
            }),
        }
    }

    pub(crate) fn replace_bridge(
        &mut self,
        bridge: Arc<dyn Bridge>,
        supported_formats: &[ApiFormat],
    ) -> Result<(), ConfigurationError> {
        let checked = Self::new(bridge, supported_formats, self.options.clone())?;
        self.bridge = checked.bridge;
        Ok(())
    }

    pub(crate) fn set_bridge(&mut self, bridge: Arc<dyn Bridge>) {
        self.bridge = bridge;
    }

    pub(crate) fn allow_loss(&mut self, scopes: impl IntoIterator<Item = relay_bridge::LossScope>) {
        self.options.allow_loss.extend(scopes);
    }

    pub fn with_id_generator(
        mut self,
        generator: impl Fn() -> String + Send + Sync + 'static,
    ) -> Self {
        self.id_generator = Arc::new(generator);
        self
    }

    pub fn with_clock(mut self, clock: impl Fn() -> u64 + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    pub fn decode_context(&self) -> DecodeContext {
        let observed_at = (self.clock)();
        DecodeContext {
            response_id: Some((self.id_generator)()),
            observed_at,
        }
    }

    pub fn bridge(&self) -> &dyn Bridge {
        self.bridge.as_ref()
    }

    pub fn options(&self) -> &ConversionOptions {
        &self.options
    }
}
