use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};

use crate::metrics::{CoreUsageEntry, CpuUsageEntry, GpuUsageEntry, Metrics};

#[derive(Serialize)]
struct CpuUsageValue<'a> {
  units: u32,
  freq_mhz: u32,
  usage: f32,
  cores: CorePairs<'a>,
}

struct CorePairs<'a>(&'a [CoreUsageEntry]);

impl Serialize for CorePairs<'_> {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
    for core in self.0 {
      seq.serialize_element(&(core.freq_mhz, core.usage))?;
    }
    seq.end()
  }
}

struct CpuUsageMap<'a>(&'a [CpuUsageEntry]);

impl Serialize for CpuUsageMap<'_> {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    let mut map = serializer.serialize_map(Some(self.0.len()))?;
    for entry in self.0 {
      map.serialize_entry(
        &entry.name,
        &CpuUsageValue {
          units: entry.units,
          freq_mhz: entry.freq_mhz,
          usage: entry.usage,
          cores: CorePairs(&entry.cores),
        },
      )?;
    }
    map.end()
  }
}

#[derive(Serialize)]
struct GpuUsageValue {
  units: u32,
  freq_mhz: u32,
  usage: f32,
}

struct GpuUsageMap<'a>(&'a [GpuUsageEntry]);

impl Serialize for GpuUsageMap<'_> {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    let mut map = serializer.serialize_map(Some(self.0.len()))?;
    for entry in self.0 {
      map.serialize_entry(
        &entry.name,
        &GpuUsageValue { units: entry.units, freq_mhz: entry.freq_mhz, usage: entry.usage },
      )?;
    }
    map.end()
  }
}

impl Serialize for Metrics {
  fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    let mut map = serializer.serialize_map(Some(5))?;
    map.serialize_entry("temp", &self.temp)?;
    map.serialize_entry("memory", &self.memory)?;
    map.serialize_entry("cpu_usage", &CpuUsageMap(&self.cpu_usage))?;
    map.serialize_entry("gpu_usage", &GpuUsageMap(&self.gpu_usage))?;
    map.serialize_entry("power", &self.power)?;
    map.end()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::metrics::{MemMetrics, PowerMetrics, TempMetrics};

  #[test]
  fn metrics_serialize_with_cli_shape() {
    let metrics = Metrics {
      temp: TempMetrics { cpu_avg: 43.0, gpu_avg: 37.0 },
      memory: MemMetrics { ram_total: 1, ram_usage: 2, swap_total: 3, swap_usage: 4 },
      cpu_usage: vec![CpuUsageEntry {
        name: "ECPU".into(),
        units: 2,
        freq_mhz: 1200,
        usage: 0.25,
        cores: vec![
          CoreUsageEntry { freq_mhz: 1000, usage: 0.2 },
          CoreUsageEntry { freq_mhz: 1400, usage: 0.3 },
        ],
      }],
      gpu_usage: vec![GpuUsageEntry {
        name: "GPUPH".into(),
        units: 10,
        freq_mhz: 461,
        usage: 0.02,
      }],
      power: PowerMetrics {
        package: 0.321,
        cpu: 0.2,
        gpu: 0.01,
        ram: 0.11,
        gpu_ram: 0.001,
        ane: 0.0,
        board: 5.8,
        battery: 0.7,
        dc_in: 0.8,
      },
    };

    let value = serde_json::to_value(metrics).unwrap();

    assert_eq!(value["cpu_usage"]["ECPU"]["units"], 2);
    assert_eq!(value["cpu_usage"]["ECPU"]["freq_mhz"], 1200);
    assert!((value["cpu_usage"]["ECPU"]["usage"].as_f64().unwrap() - 0.25).abs() < 1e-6);
    assert_eq!(value["cpu_usage"]["ECPU"]["cores"][0][0], 1000);
    assert!((value["cpu_usage"]["ECPU"]["cores"][0][1].as_f64().unwrap() - 0.2).abs() < 1e-6);
    assert_eq!(value["gpu_usage"]["GPUPH"]["freq_mhz"], 461);
    assert_eq!(value["gpu_usage"]["GPUPH"]["units"], 10);
    assert!((value["gpu_usage"]["GPUPH"]["usage"].as_f64().unwrap() - 0.02).abs() < 1e-6);
    assert!((value["power"]["package"].as_f64().unwrap() - 0.321).abs() < 1e-6);
    assert_eq!(value["memory"]["swap_usage"], 4);
    assert!((value["temp"]["cpu_avg"].as_f64().unwrap() - 43.0).abs() < 1e-6);
  }
}
