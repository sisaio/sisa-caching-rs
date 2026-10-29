use sisa_caching_core::cache::CacheValue;

/// Turns values into what the store keeps, and back.
pub(crate) trait CacheCodec<CV>: Clone + Send + Sync + 'static {
    type EncodedValue: Send + Sync + 'static;
    type Error: std::error::Error + Send + Sync + 'static;

    fn encode(&self, value: &CV) -> Result<Self::EncodedValue, Self::Error>;

    fn decode(&self, encoded_value: &Self::EncodedValue) -> Result<CV, Self::Error>;
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct JsonCodec;

impl<CV: CacheValue> CacheCodec<CV> for JsonCodec {
    type EncodedValue = Vec<u8>;
    type Error = serde_json::Error;

    fn encode(&self, value: &CV) -> Result<Self::EncodedValue, Self::Error> {
        serde_json::to_vec(value)
    }

    fn decode(&self, encoded_value: &Self::EncodedValue) -> Result<CV, Self::Error> {
        serde_json::from_slice(encoded_value)
    }
}
