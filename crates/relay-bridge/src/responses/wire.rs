use relay_protocol::{ConversionError, ConversionErrorKind, ConversionLoss, DecodeError};
use serde_json::{Map, Value};

use crate::{BridgeError, ConversionOptions, LossScope};

pub(super) fn invalid(path: &str, message: impl Into<String>) -> BridgeError {
    ConversionError {
        kind: ConversionErrorKind::Invalid,
        path: path.into(),
        message: message.into(),
    }
    .into()
}

pub(super) fn unrepresentable(path: &str, message: impl Into<String>) -> BridgeError {
    ConversionError {
        kind: ConversionErrorKind::Unrepresentable,
        path: path.into(),
        message: message.into(),
    }
    .into()
}

pub(super) fn decode_error(path: &str, message: impl Into<String>) -> BridgeError {
    DecodeError {
        path: path.into(),
        message: message.into(),
    }
    .into()
}

macro_rules! parse {
    ($ty:ty; $value:expr, $path:expr) => {
        serde_json::from_value::<$ty>($value).map_err(|error| invalid($path, error.to_string()))
    };
    ($value:expr, $path:expr) => {
        serde_json::from_value($value).map_err(|error| invalid($path, error.to_string()))
    };
}

pub(super) fn object(value: &Value, path: &str) -> Result<Map<String, Value>, BridgeError> {
    value
        .as_object()
        .cloned()
        .ok_or_else(|| invalid(path, "expected an object"))
}

pub(super) fn array<'a>(value: &'a Value, path: &str) -> Result<&'a [Value], BridgeError> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| invalid(path, "expected an array"))
}

macro_rules! take {
    ($ty:ty; $object:expr, $key:expr, $path:expr) => {
        parse!($ty; $object.remove($key).unwrap_or(Value::Null), &format!("{}/{}", $path, $key))
    };
    ($object:expr, $key:expr, $path:expr) => {
        parse!($object.remove($key).unwrap_or(Value::Null), &format!("{}/{}", $path, $key))
    };
}

macro_rules! optional {
    ($ty:ty; $object:expr, $key:expr, $path:expr) => {
        parse!(Option<$ty>; $object.remove($key).unwrap_or(Value::Null), &format!("{}/{}", $path, $key))
    };
    ($object:expr, $key:expr, $path:expr) => {
        parse!(Option<_>; $object.remove($key).unwrap_or(Value::Null), &format!("{}/{}", $path, $key))
    };
}

pub(super) fn reject_remaining(object: &Map<String, Value>, path: &str) -> Result<(), BridgeError> {
    if let Some(key) = object.keys().next() {
        return Err(unrepresentable(
            &format!("{path}/{}", pointer_key(key)),
            "field has no protocol representation",
        ));
    }
    Ok(())
}

pub(super) fn allow_keys(
    object: &Map<String, Value>,
    keys: &[&str],
    path: &str,
) -> Result<(), BridgeError> {
    if let Some(key) = object.keys().find(|key| !keys.contains(&key.as_str())) {
        return Err(unrepresentable(
            &format!("{path}/{}", pointer_key(key)),
            "field has no protocol representation",
        ));
    }
    Ok(())
}

pub(super) fn pointer_key(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

macro_rules! put {
    ($object:expr, $key:expr, $value:expr) => {
        if let Some(value) = $value {
            $object.insert($key.into(), serde_json::json!(value));
        }
    };
}

pub(super) fn drop_allowed(
    scope: LossScope,
    path: &str,
    options: &ConversionOptions,
    losses: &mut Vec<ConversionLoss>,
) -> Result<(), BridgeError> {
    if !options.allow_loss.contains(&scope) {
        return Err(unrepresentable(
            path,
            "field cannot be represented by Responses",
        ));
    }
    losses.push(ConversionLoss {
        path: path.into(),
        message: "field omitted for Responses".into(),
    });
    Ok(())
}

pub(super) fn decode_object(value: &Value, path: &str) -> Result<Map<String, Value>, BridgeError> {
    value
        .as_object()
        .cloned()
        .ok_or_else(|| decode_error(path, "expected an object"))
}

macro_rules! decoded {
    ($ty:ty; $value:expr, $path:expr) => {
        serde_json::from_value::<$ty>($value)
            .map_err(|error| decode_error($path, error.to_string()))
    };
    ($value:expr, $path:expr) => {
        serde_json::from_value($value).map_err(|error| decode_error($path, error.to_string()))
    };
}

macro_rules! field {
    ($ty:ty; $value:expr, $key:expr, $path:expr) => {
        decoded!($ty; $value.get($key).cloned().unwrap_or(Value::Null), &format!("{}/{}", $path, $key))
    };
    ($value:expr, $key:expr, $path:expr) => {
        decoded!($value.get($key).cloned().unwrap_or(Value::Null), &format!("{}/{}", $path, $key))
    };
}

pub(super) use {decoded, field, optional, parse, put, take};
