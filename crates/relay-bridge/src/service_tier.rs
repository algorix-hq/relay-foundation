use relay_protocol::{ConversionError, ConversionErrorKind, ServiceTier};
use serde_json::Value;

pub(crate) struct ServiceTiers(&'static [(ServiceTier, &'static str)]);

pub(crate) const OPENAI: ServiceTiers = ServiceTiers(&[
    (ServiceTier::Auto, "auto"),
    (ServiceTier::Standard, "default"),
    (ServiceTier::Flex, "flex"),
    (ServiceTier::Scale, "scale"),
    (ServiceTier::Priority, "priority"),
]);

pub(crate) const MESSAGES: ServiceTiers = ServiceTiers(&[
    (ServiceTier::Auto, "auto"),
    (ServiceTier::Standard, "standard_only"),
]);

impl ServiceTiers {
    pub(crate) fn encode(&self, tier: &ServiceTier) -> Result<&'static str, ConversionError> {
        self.0
            .iter()
            .find(|(t, _)| t == tier)
            .map(|(_, s)| *s)
            .ok_or_else(|| ConversionError {
                kind: ConversionErrorKind::Unrepresentable,
                path: "/parameters/service_tier".into(),
                message: format!("service tier {tier:?} has no wire representation"),
            })
    }

    pub(crate) fn decode(&self, value: Value) -> Result<ServiceTier, ConversionError> {
        let wire = value.as_str().ok_or_else(|| ConversionError {
            kind: ConversionErrorKind::Invalid,
            path: "/service_tier".into(),
            message: "expected a string".into(),
        })?;
        self.0
            .iter()
            .find(|(_, s)| *s == wire)
            .map(|(tier, _)| tier.clone())
            .ok_or_else(|| ConversionError {
                kind: ConversionErrorKind::Unrepresentable,
                path: "/service_tier".into(),
                message: format!("unsupported wire service tier {wire:?}"),
            })
    }
}
