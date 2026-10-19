//! Usage events, only ever recorded when the user turned `rustcode.telemetry.enabled` on.

/// Where usage events go. Implemented by infrastructure. An event has a name and no personal
/// data: the caller adds only the version of the program and the platform.
pub trait Telemetry: Send + Sync {
    fn record(&self, event: &str);
}

/// Records nothing (tests, and the default until the composition root gives a real one).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoTelemetry;

impl Telemetry for NoTelemetry {
    fn record(&self, _: &str) {}
}
