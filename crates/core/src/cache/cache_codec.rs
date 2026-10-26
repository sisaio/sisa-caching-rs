/// Turns values into what a store keeps, such as JSON for Redis, and back.
#[doc(hidden)]
pub trait CacheCodec<CV>: Clone + Send + Sync + 'static {
    type EncodedValue: Send + Sync + 'static;
    type Error: std::error::Error + Send + Sync + 'static;

    fn encode(&self, value: &CV) -> Result<Self::EncodedValue, Self::Error>;

    fn decode(&self, encoded_value: &Self::EncodedValue) -> Result<CV, Self::Error>;
}
