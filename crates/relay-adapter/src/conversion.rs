use std::{error::Error, fmt, sync::Arc};

use relay_bridge::{ApiFormat, Bridge, ConversionOptions};

pub struct Conversion {
    bridge: Arc<dyn Bridge>,
    options: ConversionOptions,
}

impl Conversion {
    pub fn new(
        bridge: Arc<dyn Bridge>,
        supported_formats: &[ApiFormat],
        options: ConversionOptions,
    ) -> Result<Self, IncompatibleFormat> {
        if !supported_formats.contains(bridge.format()) {
            return Err(IncompatibleFormat {
                bridge_format: bridge.format().clone(),
                supported_formats: supported_formats.to_vec(),
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
