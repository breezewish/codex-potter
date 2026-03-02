use serde::Deserialize;
use serde::Serialize;

/// ID of a request, which can be either a string or an integer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    String(String),
    Integer(i64),
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestId::String(value) => f.write_str(value),
            RequestId::Integer(value) => value.fmt(f),
        }
    }
}
