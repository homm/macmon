//! Selected SMC sensors and cached hardware snapshots.

use std::time::{Duration, Instant};

use crate::shared::{FanMetric, TempMetrics};
use crate::sources::{SMC, SmcKey, WithError, sysctl_str};

// CPU/GPU temperature keys adapted from exelban/stats, Modules/Sensors/values.swift.
// Retrieved 2026-11-05. Copyright (c) 2019, 2020 Serhiy Mytrovtsiy.
// Distributed under the MIT license included in LICENSE.
fn temperature_keys(chip: &str) -> Option<(&'static [&'static str], &'static [&'static str])> {
  match chip {
    "Apple M1" | "Apple M1 Pro" | "Apple M1 Max" | "Apple M1 Ultra" => Some((
      &["Tp09", "Tp0T", "Tp01", "Tp05", "Tp0D", "Tp0H", "Tp0L", "Tp0P", "Tp0X", "Tp0b"],
      &["Tg05", "Tg0D", "Tg0L", "Tg0T"],
    )),
    "Apple M2" | "Apple M2 Pro" | "Apple M2 Max" | "Apple M2 Ultra" => Some((
      &[
        "Tp1h", "Tp1t", "Tp1p", "Tp1l", "Tp01", "Tp05", "Tp09", "Tp0D", "Tp0X", "Tp0b", "Tp0f",
        "Tp0j",
      ],
      &["Tg0f", "Tg0j"],
    )),
    "Apple M3" | "Apple M3 Pro" | "Apple M3 Max" | "Apple M3 Ultra" => Some((
      &[
        "Te05", "Te0L", "Te0P", "Te0S", "Tf04", "Tf09", "Tf0A", "Tf0B", "Tf0D", "Tf0E", "Tf44",
        "Tf49", "Tf4A", "Tf4B", "Tf4D", "Tf4E",
      ],
      &["Tf14", "Tf18", "Tf19", "Tf1A", "Tf24", "Tf28", "Tf29", "Tf2A"],
    )),
    "Apple M4" => Some((
      &[
        "Te05", "Te0S", "Te09", "Te0H", "Tp01", "Tp05", "Tp09", "Tp0D", "Tp0V", "Tp0Y", "Tp0b",
        "Tp0e",
      ],
      &["Tg0G", "Tg0H", "Tg0K", "Tg0L", "Tg0d", "Tg0e", "Tg0j", "Tg0k"],
    )),
    "Apple M4 Pro" | "Apple M4 Max" => Some((
      &[
        "Te05", "Te0S", "Te09", "Te0H", "Tp01", "Tp05", "Tp09", "Tp0D", "Tp0V", "Tp0Y", "Tp0b",
        "Tp0e",
      ],
      &["Tg1U", "Tg1k", "Tg0K", "Tg0L", "Tg0d", "Tg0e", "Tg0j", "Tg0k"],
    )),
    "Apple M5" | "Apple M5 Pro" | "Apple M5 Max" | "Apple M5 Ultra" => Some((
      &[
        "Tp00", "Tp04", "Tp08", "Tp0C", "Tp0G", "Tp0K", "Tp0O", "Tp0R", "Tp0U", "Tp0X", "Tp0a",
        "Tp0d", "Tp0g", "Tp0j", "Tp0m", "Tp0p", "Tp0u", "Tp0y",
      ],
      &["Tg0U", "Tg0X", "Tg0d", "Tg0g", "Tg0j", "Tg1Y", "Tg1c", "Tg1g"],
    )),
    "Apple M6" | "Apple M6 Pro" | "Apple M6 Max" => Some((
      &[
        "Tp0j", "Tp0g", "Tp09", "Tp07", "Tp0m", "Tp0L", "Tp0I", "Tp0d", "Tp0E", "Tp0G", "Tp0b",
        "Tp05", "Te07", "Te08", "Te09", "Te0c", "Te0h", "Te0i",
      ],
      &[
        "Tg1e", "Tg0d", "Tg0c", "Tg0b", "Tg1c", "Tg1d", "Tg07", "Tg1f", "Tg09", "Tg0a", "Tg1g",
        "Tg1h",
      ],
    )),
    "Apple A18 Pro" => Some((&["Te05", "Te0S", "Tp05", "Tp0D"], &["Tg05", "Tg0D", "Tg0L", "Tg0e"])),
    _ => None,
  }
}

/// One snapshot of the SMC metrics used by the sampler.
#[derive(Debug, Default, Clone)]
pub struct SmcMetrics {
  /// Average CPU and GPU temperatures in Celsius. None when no CPU sensors were selected.
  pub temp: Option<TempMetrics>,
  /// Fans in stable SMC key order.
  pub fans: Vec<FanMetric>,
  /// System power in Watts, or None when unavailable.
  pub sys_power: Option<f32>,
  /// Display backlight power in Watts, or None when unavailable.
  pub display_power: Option<f32>,
  /// Battery power in Watts, or None when unavailable.
  pub battery_power: Option<f32>,
  /// DC input power in Watts, or None when unavailable.
  pub dc_in_power: Option<f32>,
}

/// Selects SMC sensors, computes metrics, and caches complete snapshots.
pub struct SmcSampler {
  smc: Option<SMC>,
  cpu_keys: Vec<SmcKey>,
  gpu_keys: Vec<SmcKey>,
  fan_keys: Vec<(SmcKey, SmcKey)>,
  power_key: SmcKey,
  display_keys: [SmcKey; 2],
  battery_key: SmcKey,
  dc_in_key: SmcKey,
  cache_duration: Duration,
  cached: Option<(Instant, SmcMetrics)>,
}

impl SmcSampler {
  /// Discover available sensors. None uses a one-second cache lifetime.
  pub fn new(cache_duration: Option<Duration>) -> WithError<Self> {
    let mut result = Self {
      smc: SMC::open()?,
      cpu_keys: Vec::new(),
      gpu_keys: Vec::new(),
      fan_keys: Vec::new(),
      power_key: SmcKey::new("PSTR")?,
      display_keys: [SmcKey::new("PBwo")?, SmcKey::new("PDBR")?],
      battery_key: SmcKey::new("PPBR")?,
      dc_in_key: SmcKey::new("PDTR")?,
      cache_duration: cache_duration.unwrap_or(Duration::from_secs(1)),
      cached: None,
    };
    if let Some(smc) = result.smc.as_mut() {
      let chip = sysctl_str("machdep.cpu.brand_string").unwrap_or_default();
      let keys = temperature_keys(&chip);
      let mut fan_names = Vec::new();
      for name in smc.read_all_keys().unwrap_or_default() {
        if name.len() == 4 && name.starts_with('F') && name.ends_with("Ac") {
          fan_names.push(name);
          continue;
        }
        let (is_cpu, is_gpu) = match keys {
          Some((cpu, gpu)) => (cpu.contains(&name.as_str()), gpu.contains(&name.as_str())),
          None => (
            name.starts_with("Tp") || name.starts_with("Te") || name.starts_with("Ts"),
            name.starts_with("Tg"),
          ),
        };
        if !is_cpu && !is_gpu {
          continue;
        }
        if smc.read_float_val(&name).is_err() {
          continue;
        }
        if is_cpu {
          result.cpu_keys.push(smc.prepare_key(&name)?);
        } else {
          result.gpu_keys.push(smc.prepare_key(&name)?);
        }
      }
      // Stable fan order, with duplicate keys adjacent for deduplication.
      fan_names.sort();
      fan_names.dedup();
      for name in fan_names {
        let max_name = format!("{}Mx", &name[..2]);
        result.fan_keys.push((SmcKey::new(&name)?, SmcKey::new(&max_name)?));
      }
    }
    Ok(result)
  }

  /// Set snapshot lifetime and discard the previous snapshot. Zero disables caching.
  pub fn set_cache_duration(&mut self, duration: Duration) {
    self.cache_duration = duration;
    self.cached = None;
  }

  /// Return the cached snapshot until it expires, then read all selected sensors.
  /// Collection errors do not replace the cache or extend its lifetime.
  pub fn get_metrics(&mut self) -> WithError<SmcMetrics> {
    if let Some((sampled_at, metrics)) = &self.cached
      && sampled_at.elapsed() < self.cache_duration
    {
      return Ok(metrics.clone());
    }
    let temp = if self.cpu_keys.is_empty() { None } else { Some(self.get_temp_smc()?) };
    let fans = self.get_fans();
    let read_power = |key: &mut SmcKey| {
      self.smc.as_ref()?.read_float_key(key).ok().filter(|value| value.is_finite())
    };
    let sys_power = read_power(&mut self.power_key);
    // Backlight sensor names used by different Apple Silicon generations.
    let display_power =
      read_power(&mut self.display_keys[0]).or_else(|| read_power(&mut self.display_keys[1]));
    let battery_power = read_power(&mut self.battery_key);
    let dc_in_power = read_power(&mut self.dc_in_key);
    let metrics = SmcMetrics { temp, fans, sys_power, display_power, battery_power, dc_in_power };
    if !self.cache_duration.is_zero() {
      self.cached = Some((Instant::now(), metrics.clone()));
    }
    Ok(metrics)
  }

  pub fn get_temp_smc(&mut self) -> WithError<TempMetrics> {
    let smc = self.smc.as_mut().ok_or("SMC unavailable")?;
    let mut averages = [None; 2];
    for (keys, average) in [&mut self.cpu_keys, &mut self.gpu_keys].into_iter().zip(&mut averages) {
      let mut sum = 0.0;
      let mut count = 0;
      for key in keys {
        let value = smc.read_float_key(key)?;
        if value > 0.0 && value <= 150.0 {
          sum += value;
          count += 1;
        }
      }
      *average = (count > 0).then(|| sum / count as f32);
    }
    Ok(TempMetrics { cpu_temp_avg: averages[0], gpu_temp_avg: averages[1] })
  }

  fn get_fans(&mut self) -> Vec<FanMetric> {
    let mut fans = Vec::new();
    if let Some(smc) = self.smc.as_mut() {
      let read_rpm = |key: &mut SmcKey| {
        let value = smc.read_numeric_key(key).ok()?;
        if (0.0..=100_000.0).contains(&value) { Some(value.trunc() as u32) } else { None }
      };
      for (i, (key, max_key)) in self.fan_keys.iter_mut().enumerate() {
        if let Some(rpm) = read_rpm(key) {
          let name = format!("fan{i}");
          let max_rpm = read_rpm(max_key).filter(|rpm| *rpm > 0);
          fans.push(FanMetric { name, rpm, max_rpm });
        }
      }
    }
    fans
  }
}

#[cfg(test)]
pub(crate) mod tests {
  use super::*;

  #[test]
  fn temperature_tables_distinguish_m4_variants_and_unknown_chips() {
    let (cpu, gpu) = temperature_keys("Apple M4 Pro").unwrap();
    assert!(cpu.contains(&"Te05") && cpu.contains(&"Tp0e"));
    assert!(gpu.contains(&"Tg1U") && gpu.contains(&"Tg1k"));
    assert!(!gpu.contains(&"Tg0G") && !gpu.contains(&"Tg0H"));
    let (_, base_gpu) = temperature_keys("Apple M4").unwrap();
    assert!(base_gpu.contains(&"Tg0G") && !base_gpu.contains(&"Tg1U"));
    let (cpu, gpu) = temperature_keys("Apple M3 Ultra").unwrap();
    assert!(cpu.contains(&"Tf4E") && gpu.contains(&"Tf2A"));
    for chip in ["Apple M4 Ultra", "Apple M10", "Apple M7", "", "Virtual CPU"] {
      assert!(temperature_keys(chip).is_none(), "{chip}");
    }
  }

  pub(crate) fn without_sources() -> SmcSampler {
    SmcSampler {
      smc: None,
      cpu_keys: Vec::new(),
      gpu_keys: Vec::new(),
      fan_keys: Vec::new(),
      power_key: SmcKey::new("PSTR").unwrap(),
      display_keys: [SmcKey::new("PBwo").unwrap(), SmcKey::new("PDBR").unwrap()],
      battery_key: SmcKey::new("PPBR").unwrap(),
      dc_in_key: SmcKey::new("PDTR").unwrap(),
      cache_duration: Duration::from_secs(1),
      cached: None,
    }
  }

  #[test]
  fn snapshot_is_reused_until_expiration_without_extending_its_lifetime() {
    let mut smc = without_sources();
    let sampled_at = Instant::now();
    smc.cached = Some((
      sampled_at,
      SmcMetrics {
        temp: Some(TempMetrics { cpu_temp_avg: Some(45.0), gpu_temp_avg: Some(40.0) }),
        fans: vec![FanMetric { name: "fan0".into(), rpm: 1200, max_rpm: Some(6000) }],
        sys_power: Some(12.0),
        display_power: Some(2.0),
        battery_power: Some(-3.0),
        dc_in_power: Some(15.0),
      },
    ));
    for _ in 0..2 {
      let metrics = smc.get_metrics().unwrap();
      let temp = metrics.temp.unwrap();
      assert_eq!(temp.cpu_temp_avg, Some(45.0));
      assert_eq!(temp.gpu_temp_avg, Some(40.0));
      assert_eq!(metrics.fans[0].rpm, 1200);
      assert_eq!(metrics.fans[0].max_rpm, Some(6000));
      assert_eq!(metrics.sys_power, Some(12.0));
      assert_eq!(metrics.display_power, Some(2.0));
      assert_eq!(metrics.battery_power, Some(-3.0));
      assert_eq!(metrics.dc_in_power, Some(15.0));
      assert_eq!(smc.cached.as_ref().unwrap().0, sampled_at);
    }
    smc.cached.as_mut().unwrap().0 = Instant::now() - Duration::from_secs(1);
    let refreshed = smc.get_metrics().unwrap();
    assert!(refreshed.temp.is_none());
    assert!(refreshed.fans.is_empty());
    assert_eq!(refreshed.sys_power, None);
  }

  #[test]
  fn temperatures_require_cpu_sensors_even_when_gpu_sensors_exist() {
    let mut smc = without_sources();
    smc.gpu_keys.push(SmcKey::new("Tg01").unwrap());
    for _ in 0..2 {
      assert!(smc.get_metrics().unwrap().temp.is_none());
    }
  }

  #[test]
  fn unavailable_readings_in_cached_temperature_do_not_request_fallback() {
    let mut smc = without_sources();
    smc.cached = Some((
      Instant::now(),
      SmcMetrics { temp: Some(TempMetrics::default()), ..Default::default() },
    ));
    let temp = smc.get_metrics().unwrap().temp.unwrap();
    assert_eq!(temp.cpu_temp_avg, None);
    assert_eq!(temp.gpu_temp_avg, None);
  }

  #[test]
  fn setting_zero_discards_snapshot_and_disables_caching() {
    let mut smc = without_sources();
    smc.get_metrics().unwrap();
    assert!(smc.cached.is_some());
    smc.set_cache_duration(Duration::ZERO);
    assert!(smc.cached.is_none());
    for _ in 0..2 {
      smc.get_metrics().unwrap();
      assert!(smc.cached.is_none());
    }
  }

  #[test]
  fn failed_refresh_does_not_serve_or_renew_expired_snapshot() {
    let mut smc = without_sources();
    let sampled_at = Instant::now() - Duration::from_secs(2);
    smc.cached = Some((sampled_at, SmcMetrics::default()));
    smc.cpu_keys.push(SmcKey::new("Tp01").unwrap());
    for _ in 0..2 {
      assert!(smc.get_metrics().is_err());
      assert_eq!(smc.cached.as_ref().unwrap().0, sampled_at);
    }
  }
}
