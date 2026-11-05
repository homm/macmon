//! Selected SMC sensors and cached hardware snapshots.

use std::time::{Duration, Instant};

use crate::shared::{FanMetric, TempMetrics, temperature_average};
use crate::sources::{SMC, WithError};

/// One snapshot of the SMC metrics used by the sampler.
#[derive(Debug, Default, Clone)]
pub struct SmcMetrics {
  /// Average CPU and GPU temperatures in Celsius. None when no CPU sensors were selected.
  pub temp: Option<TempMetrics>,
  /// Fans in stable SMC key order.
  pub fans: Vec<FanMetric>,
  /// System power in Watts, or None when unavailable.
  pub sys_power: Option<f32>,
}

/// Selects SMC sensors, computes metrics, and caches complete snapshots.
pub struct SmcSampler {
  smc: Option<SMC>,
  cpu_keys: Vec<String>,
  gpu_keys: Vec<String>,
  fan_keys: Vec<String>,
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
      cache_duration: cache_duration.unwrap_or(Duration::from_secs(1)),
      cached: None,
    };
    if let Some(smc) = result.smc.as_mut() {
      for name in smc.read_all_keys().unwrap_or_default() {
        if name.len() == 4 && name.starts_with('F') && name.ends_with("Ac") {
          result.fan_keys.push(name);
          continue;
        }
        // Exact sensor assignments are undocumented; retain the existing CPU/GPU prefixes.
        let is_cpu = name.starts_with("Tp") || name.starts_with("Te") || name.starts_with("Ts");
        let is_gpu = name.starts_with("Tg");
        if !is_cpu && !is_gpu {
          continue;
        }
        if smc.read_float_val(&name).is_err() {
          continue;
        }
        if is_cpu {
          result.cpu_keys.push(name);
        } else {
          result.gpu_keys.push(name);
        }
      }
    }
    // Stable fan order, with duplicate keys adjacent for deduplication.
    result.fan_keys.sort();
    result.fan_keys.dedup();
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
    let sys_power = self.smc.as_mut().and_then(|smc| smc.read_float_val("PSTR").ok());
    let metrics = SmcMetrics { temp, fans, sys_power };
    if !self.cache_duration.is_zero() {
      self.cached = Some((Instant::now(), metrics.clone()));
    }
    Ok(metrics)
  }

  pub fn get_temp_smc(&mut self) -> WithError<TempMetrics> {
    let smc = self.smc.as_mut().ok_or("SMC unavailable")?;
    let mut cpu_metrics = Vec::new();
    for sensor in &self.cpu_keys {
      cpu_metrics.push(smc.read_float_val(sensor)?);
    }
    let mut gpu_metrics = Vec::new();
    for sensor in &self.gpu_keys {
      gpu_metrics.push(smc.read_float_val(sensor)?);
    }
    Ok(TempMetrics {
      cpu_temp_avg: temperature_average(&cpu_metrics),
      gpu_temp_avg: temperature_average(&gpu_metrics),
    })
  }

  fn get_fans(&mut self) -> Vec<FanMetric> {
    let mut fans = Vec::new();
    if let Some(smc) = self.smc.as_mut() {
      let mut read_rpm = |key: &str| {
        let value = smc.read_numeric_value(key).ok()?;
        if (0.0..=100_000.0).contains(&value) { Some(value.trunc() as u32) } else { None }
      };
      for (i, key) in self.fan_keys.iter().enumerate() {
        if let Some(rpm) = read_rpm(key) {
          let name = format!("fan{i}");
          let max_rpm = match key.strip_suffix("Ac") {
            Some(prefix) => read_rpm(&format!("{prefix}Mx")).filter(|rpm| *rpm > 0),
            None => None,
          };
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

  pub(crate) fn without_sources() -> SmcSampler {
    SmcSampler {
      smc: None,
      cpu_keys: Vec::new(),
      gpu_keys: Vec::new(),
      fan_keys: Vec::new(),
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
    smc.gpu_keys.push("Tg01".into());
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
    smc.cpu_keys.push("Tp01".into());
    for _ in 0..2 {
      assert!(smc.get_metrics().is_err());
      assert_eq!(smc.cached.as_ref().unwrap().0, sampled_at);
    }
  }
}
