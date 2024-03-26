use nomos_metrics::{metrics::gauge::Gauge, NomosRegistry};
use overwatch_rs::services::ServiceId;

pub(crate) struct Metrics {
    mixing_packets: Gauge,
}

impl Metrics {
    pub(crate) fn new(registry: NomosRegistry, discriminant: ServiceId) -> Self {
        let mut registry = registry
            .lock()
            .expect("should've acquired the lock for registry");
        let sub_registry = registry.sub_registry_with_prefix(discriminant);

        let mixing_packets = Gauge::default();
        sub_registry.register(
            "mixing_packets",
            "Number of packets being mixed in the mixnode",
            mixing_packets.clone(),
        );

        Self { mixing_packets }
    }
}
