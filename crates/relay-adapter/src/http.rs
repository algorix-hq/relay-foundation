#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

/// Preserves header order and duplicates. Lookup returns the first matching value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Headers(pub Vec<(String, String)>);

impl Headers {
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderRequest {
    pub method: Method,
    pub url: String,
    pub headers: Headers,
    /// Empty for GET requests.
    pub body: Vec<u8>,
}
