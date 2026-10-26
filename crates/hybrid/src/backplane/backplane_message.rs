use std::collections::HashMap;

use opentelemetry::global;
use serde::{Deserialize, Deserializer, Serialize, de::Error};
use sisa_caching_core::cache::KeyPrefix;

use super::node_id::NodeId;

const MESSAGE_VERSION: u8 = 1;

/// The one message published and received by every backplane. It names the writer, the store,
/// and the local copies to remove, and carries the writer's trace context so the nodes that apply
/// it continue the same trace. The same type is serialized directly for transport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackplaneMessage {
    #[serde(rename = "v", deserialize_with = "deserialize_version")]
    version: u8,
    /// The writer's node id. That node skips the message.
    pub source: NodeId,
    /// The store whose local copies go.
    pub store: KeyPrefix,
    /// Which local copies go.
    #[serde(flatten)]
    pub invalidation: Invalidation,
    /// The writer's trace context as OpenTelemetry propagation fields, such as W3C `traceparent`.
    /// Empty when the writer had no trace or no propagator is installed.
    #[serde(rename = "trace", default, skip_serializing_if = "HashMap::is_empty")]
    pub trace_context: HashMap<String, String>,
}

impl BackplaneMessage {
    /// A message carrying the current trace context, injected by the global text map propagator.
    pub fn new(source: NodeId, store: KeyPrefix, invalidation: Invalidation) -> Self {
        let mut trace_context = HashMap::new();

        global::get_text_map_propagator(|propagator| propagator.inject(&mut trace_context));

        Self {
            version: MESSAGE_VERSION,
            source,
            store,
            invalidation,
            trace_context,
        }
    }

    /// The writer's trace context, for the receiver to continue its trace.
    pub(crate) fn parent_context(&self) -> opentelemetry::Context {
        global::get_text_map_propagator(|propagator| propagator.extract(&self.trace_context))
    }
}

/// The local copies an invalidation removes. These correspond to a memory store's `remove`,
/// `remove_prefix`, and `clear` operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "target", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Invalidation {
    /// One key.
    Key(String),
    /// Every key under a prefix.
    Prefix(KeyPrefix),
    /// Every key in the store.
    Clear,
}

impl Invalidation {
    /// The wire name of the kind: `key`, `prefix` or `clear`. Info spans and warnings carry this
    /// instead of the key, so they don't export cache keys.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Key(_) => "key",
            Self::Prefix(_) => "prefix",
            Self::Clear => "clear",
        }
    }
}

fn deserialize_version<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u8, D::Error> {
    let version = u8::deserialize(deserializer)?;

    if version != MESSAGE_VERSION {
        return Err(D::Error::custom(format_args!(
            "backplane message version {version} is not supported; expected {MESSAGE_VERSION}"
        )));
    }

    Ok(version)
}

#[cfg(test)]
mod tests {
    use opentelemetry::{
        Context,
        trace::{SpanContext, SpanId, TraceContextExt, TraceFlags, TraceId, TraceState},
    };
    use opentelemetry_sdk::propagation::TraceContextPropagator;
    use serde_json::{Value, json};

    use super::*;

    fn message(invalidation: Invalidation) -> BackplaneMessage {
        BackplaneMessage::new(
            NodeId::new("node-a"),
            KeyPrefix::new("users").unwrap(),
            invalidation,
        )
    }

    #[test]
    fn messages_keep_the_existing_json_format() {
        for (invalidation, payload) in [
            (
                Invalidation::Key("profile:eu:42".to_owned()),
                json!({"v":1,"source":"node-a","store":"users","kind":"key","target":"profile:eu:42"}),
            ),
            (
                Invalidation::Key("42".to_owned()),
                json!({"v":1,"source":"node-a","store":"users","kind":"key","target":"42"}),
            ),
            (
                Invalidation::Prefix(KeyPrefix::new("profile:eu").unwrap()),
                json!({"v":1,"source":"node-a","store":"users","kind":"prefix","target":"profile:eu"}),
            ),
            (
                Invalidation::Clear,
                json!({"v":1,"source":"node-a","store":"users","kind":"clear"}),
            ),
        ] {
            let message = message(invalidation);

            assert_eq!(serde_json::to_value(&message).unwrap(), payload);
            assert_eq!(
                serde_json::from_value::<BackplaneMessage>(payload).unwrap(),
                message
            );
        }
    }

    #[test]
    fn messages_carry_the_writers_trace() {
        let traceparent = "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01";
        let writer = SpanContext::new(
            TraceId::from_hex("4bf92f3577b34da6a3ce929d0e0e4736").unwrap(),
            SpanId::from_hex("00f067aa0ba902b7").unwrap(),
            TraceFlags::SAMPLED,
            true,
            TraceState::default(),
        );

        global::set_text_map_propagator(TraceContextPropagator::new());

        let message = {
            let _writer = Context::new()
                .with_remote_span_context(writer.clone())
                .attach();

            message(Invalidation::Clear)
        };
        let payload = json!({
            "v":1,"source":"node-a","store":"users","kind":"clear","trace":{"traceparent":traceparent,"tracestate":""}
        });

        assert_eq!(serde_json::to_value(&message).unwrap(), payload);

        let received = serde_json::from_value::<BackplaneMessage>(payload).unwrap();

        assert_eq!(
            received.parent_context().span().span_context().span_id(),
            writer.span_id()
        );
        assert_eq!(
            received.parent_context().span().span_context().trace_id(),
            writer.trace_id()
        );
    }

    #[test]
    fn unknown_or_missing_versions_are_rejected() {
        for version in [Some(json!(0)), Some(json!(2)), None] {
            let mut payload = serde_json::to_value(message(Invalidation::Clear)).unwrap();

            if let Some(version) = version {
                payload["v"] = version;
            } else {
                payload.as_object_mut().unwrap().remove("v");
            }

            assert!(serde_json::from_value::<BackplaneMessage>(payload).is_err());
        }
    }

    #[test]
    fn messages_validate_store_names_and_prefixes() {
        let mut payload =
            serde_json::to_value(message(Invalidation::Key("profile:42".to_owned()))).unwrap();
        payload["store"] = Value::from("users::");

        assert!(serde_json::from_value::<BackplaneMessage>(payload).is_err());

        let payload =
            json!({"v":1,"source":"node-a","store":"users","kind":"prefix","target":"profile::eu"});

        assert!(serde_json::from_value::<BackplaneMessage>(payload).is_err());
    }
}
