use serde::{Deserialize, Deserializer};

pub(crate) fn deserialize_non_empty<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value.is_empty() {
        return Err(serde::de::Error::custom("string must not be empty"));
    }
    Ok(value)
}

pub(crate) fn deserialize_optional_non_empty<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<String>, D::Error> {
    let value = Option::<String>::deserialize(deserializer)?;
    if value.as_ref().is_some_and(String::is_empty) {
        return Err(serde::de::Error::custom("string must not be empty"));
    }
    Ok(value)
}

pub(crate) fn deserialize_non_empty_strings<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<String>, D::Error> {
    let value = Vec::<String>::deserialize(deserializer)?;
    if value.iter().any(String::is_empty) {
        return Err(serde::de::Error::custom("string must not be empty"));
    }
    Ok(value)
}
