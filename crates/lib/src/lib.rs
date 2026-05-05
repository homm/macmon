//! macmon - Sudoless performance monitoring library for Apple Silicon processors
//!
//! This library provides access to hardware metrics from Apple Silicon processors,
//! including CPU/GPU frequencies, power consumption, temperatures, and memory usage.

pub mod ffi;
pub mod metrics;
mod metrics_json;
pub mod sources;

#[cfg(feature = "bench")]
#[doc(hidden)]
pub mod bench {
  use crate::{metrics, sources};

  pub fn ioreport_channels_filter(group: &str, subgroup: &str, channel: &str, unit: &str) -> bool {
    metrics::ioreport_channels_filter(group, subgroup, channel, unit)
  }

  pub fn init_smc() -> sources::WithError<(sources::SMC, Vec<String>, Vec<String>)> {
    metrics::init_smc()
  }
}

pub use metrics::{
  CoreUsageEntry, CpuUsageEntry, GpuUsageEntry, MemMetrics, Metrics, PowerMetrics, Sampler,
  TempMetrics, zero_div,
};
pub use sources::{CpuDomainInfo, SocInfo, get_soc_info};
