use core_foundation::dictionary::CFDictionaryRef;
use serde::Serialize;

use crate::sources::{
  CpuDomainInfo, IOHIDSensors, IOReport, SMC, SocInfo, cfio_get_residencies, get_soc_info,
  libc_ram, libc_swap,
};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

// const CPU_FREQ_DICE_SUBG: &str = "CPU Complex Performance States";
const CPU_FREQ_CORE_SUBG: &str = "CPU Core Performance States";
const GPU_FREQ_DICE_SUBG: &str = "GPU Performance States";

// MARK: Structs

#[derive(Debug, Default, Serialize)]
pub struct TempMetrics {
  pub cpu_avg: f32, // Celsius
  pub gpu_avg: f32, // Celsius
}

#[derive(Debug, Default, Serialize)]
pub struct MemMetrics {
  pub ram_total: u64,  // bytes
  pub ram_usage: u64,  // bytes
  pub swap_total: u64, // bytes
  pub swap_usage: u64, // bytes
}

#[derive(Debug, Default, Serialize)]
pub struct PowerMetrics {
  pub package: f32, // SoC/package power reported by the sampler.
  pub cpu: f32,     // CPU power included in `package`.
  pub gpu: f32,     // GPU core power included in `package`.
  pub ram: f32,     // DRAM power included in `package`.
  pub gpu_ram: f32, // GPU SRAM power included in `package`.
  pub ane: f32,     // ANE power included in `package`.
  pub board: f32,   // System Total (`PSTR`), independent from battery/DC-in readings.
  pub battery: f32, // Battery rail power (`PPBR`).
  pub dc_in: f32,   // External DC input power (`PDTR`).
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct CoreUsageEntry {
  pub freq_mhz: u32,
  pub usage: f32,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct CpuUsageEntry {
  pub name: String,
  pub units: u32,
  pub freq_mhz: u32,
  pub usage: f32,
  pub cores: Vec<CoreUsageEntry>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct GpuUsageEntry {
  pub name: String,
  pub units: u32,
  pub freq_mhz: u32,
  pub usage: f32,
}

#[derive(Debug, Default)]
pub struct Metrics {
  pub temp: TempMetrics,
  pub memory: MemMetrics,
  pub cpu_usage: Vec<CpuUsageEntry>,
  pub gpu_usage: Vec<GpuUsageEntry>,
  pub power: PowerMetrics,
}

// MARK: Helpers

pub fn zero_div<T: core::ops::Div<Output = T> + Default + PartialEq>(a: T, b: T) -> T {
  let zero: T = Default::default();
  if b == zero { zero } else { a / b }
}

fn is_valid_temp(val: f32) -> bool {
  val > 0.0 && val <= 150.0
}

fn is_active_state(name: &str) -> bool {
  // IDLE / DOWN for CPU; OFF for GPU; DOWN only on M2?/M3 Max Chips
  name != "IDLE" && name != "DOWN" && name != "OFF"
}

fn calc_freq_from_residencies(items: &[(String, i64)], freqs: &[u32]) -> (u32, f32) {
  let offset = items.iter().position(|x| is_active_state(x.0.as_str())).unwrap();
  assert!(
    items.len() >= offset + freqs.len(),
    "calc_freq invalid data: items={}, offset={}, freqs={}",
    items.len(),
    offset,
    freqs.len()
  );

  let usage = items.iter().map(|x| x.1 as f64).skip(offset).sum::<f64>();
  let total = items.iter().map(|x| x.1 as f64).sum::<f64>();
  let count = freqs.len();

  let mut avg_freq = 0f64;
  for i in 0..count {
    let percent = zero_div(items[i + offset].1 as _, usage);
    avg_freq += percent * freqs[i] as f64;
  }

  let usage_ratio = zero_div(usage, total);
  (avg_freq as u32, usage_ratio as f32)
}

fn calc_freq(item: CFDictionaryRef, freqs: &[u32]) -> (u32, f32) {
  let items = cfio_get_residencies(item); // (ns, freq)
  calc_freq_from_residencies(&items, freqs)
}

fn calc_cluster_usage_at_peak_freq(cores: &[CoreUsageEntry]) -> (u32, f32) {
  let peak_freq = cores.iter().filter(|x| x.usage > 0.0).map(|x| x.freq_mhz).max().unwrap_or(0);
  if peak_freq == 0 {
    return (0, 0.0);
  }

  let peak_freq = peak_freq as f32;
  let usage =
    zero_div(cores.iter().map(|x| x.usage * x.freq_mhz as f32 / peak_freq).sum(), cores.len() as f32);

  (peak_freq as u32, usage)
}

fn cpu_channel_domain_index(channel: &str, domains: &[CpuDomainInfo]) -> Option<usize> {
  domains.iter().position(|domain| channel.contains(domain.name.as_str()))
}

pub(crate) fn init_smc() -> WithError<(SMC, Vec<String>, Vec<String>)> {
  let mut smc = SMC::new()?;

  let mut cpu_sensors = Vec::new();
  let mut gpu_sensors = Vec::new();

  let names = smc.read_all_keys().unwrap_or(vec![]);
  for name in &names {
    // Unfortunately, it is not known which keys are responsible for what.
    // Basically in the code that can be found publicly "Tp" is used for CPU and "Tg" for GPU.

    let is_cpu = name.starts_with("Tp") || name.starts_with("Te") || name.starts_with("Ts");
    let is_gpu = name.starts_with("Tg");
    if !is_cpu && !is_gpu {
      continue;
    }

    if smc.read_float_val(name).is_err() {
      continue;
    }

    if is_cpu {
      cpu_sensors.push(name.clone());
    } else if is_gpu {
      gpu_sensors.push(name.clone());
    }
  }

  // println!("{} {}", cpu_sensors.len(), gpu_sensors.len());
  Ok((smc, cpu_sensors, gpu_sensors))
}

pub(crate) fn ioreport_channels_filter(
  group: &str,
  subgroup: &str,
  channel: &str,
  _unit: &str,
) -> bool {
  // Keep this filter in sync with the channel handling in Sampler::get_metrics.
  if group == "Energy Model" {
    return channel == "GPU Energy"
      || channel.ends_with("CPU Energy")
      || channel.starts_with("ANE")
      || channel.starts_with("DRAM")
      || channel.starts_with("GPU SRAM");
  }

  if group == "CPU Stats" {
    return subgroup == CPU_FREQ_CORE_SUBG;
  }

  group == "GPU Stats" && subgroup == GPU_FREQ_DICE_SUBG
}
// MARK: Sampler

pub struct Sampler {
  soc: SocInfo,
  ior: IOReport,
  hid: IOHIDSensors,
  smc: SMC,
  smc_cpu_keys: Vec<String>,
  smc_gpu_keys: Vec<String>,
}

impl Sampler {
  pub fn new() -> WithError<Self> {
    let soc = get_soc_info()?;
    let hid = IOHIDSensors::new()?;
    let (smc, smc_cpu_keys, smc_gpu_keys) = init_smc()?;
    // Keep IOReport initialization last: it captures the baseline for the first timed sample.
    let ior = IOReport::new(Some(ioreport_channels_filter))?;

    Ok(Sampler { soc, ior, hid, smc, smc_cpu_keys, smc_gpu_keys })
  }

  fn get_temp_smc(&mut self) -> WithError<TempMetrics> {
    let mut cpu_metrics = Vec::new();
    for sensor in &self.smc_cpu_keys {
      let val = self.smc.read_float_val(sensor)?;
      if is_valid_temp(val) {
        cpu_metrics.push(val);
      }
    }

    let mut gpu_metrics = Vec::new();
    for sensor in &self.smc_gpu_keys {
      let val = self.smc.read_float_val(sensor)?;
      if is_valid_temp(val) {
        gpu_metrics.push(val);
      }
    }

    let cpu_avg = zero_div(cpu_metrics.iter().sum::<f32>(), cpu_metrics.len() as f32);
    let gpu_avg = zero_div(gpu_metrics.iter().sum::<f32>(), gpu_metrics.len() as f32);

    Ok(TempMetrics { cpu_avg, gpu_avg })
  }

  fn get_temp_hid(&mut self) -> WithError<TempMetrics> {
    let metrics = self.hid.get_metrics();

    let mut cpu_values = Vec::new();
    let mut gpu_values = Vec::new();

    for (name, value) in &metrics {
      if name.starts_with("pACC MTR Temp Sensor") || name.starts_with("eACC MTR Temp Sensor") {
        // println!("{}: {}", name, value);
        if is_valid_temp(*value) {
          cpu_values.push(*value);
        }
        continue;
      }

      if name.starts_with("GPU MTR Temp Sensor") {
        // println!("{}: {}", name, value);
        if is_valid_temp(*value) {
          gpu_values.push(*value);
        }
        continue;
      }
    }

    let cpu_avg = zero_div(cpu_values.iter().sum(), cpu_values.len() as f32);
    let gpu_avg = zero_div(gpu_values.iter().sum(), gpu_values.len() as f32);

    Ok(TempMetrics { cpu_avg, gpu_avg })
  }

  fn get_temp(&mut self) -> WithError<TempMetrics> {
    // HID for M1, SMC for M2/M3
    // UPD: Looks like HID/SMC related to OS version, not to the chip (SMC available from macOS 14)
    match !self.smc_cpu_keys.is_empty() {
      true => self.get_temp_smc(),
      false => self.get_temp_hid(),
    }
  }

  fn get_mem(&mut self) -> WithError<MemMetrics> {
    let (ram_usage, ram_total) = libc_ram()?;
    let (swap_usage, swap_total) = libc_swap()?;
    Ok(MemMetrics { ram_total, ram_usage, swap_total, swap_usage })
  }

  pub fn get_metrics(&mut self) -> WithError<Metrics> {
    // CPU Stats channel naming by chip family (see: https://github.com/vladkens/macmon/issues/47)
    //   M1-M4:  ECPU* = efficiency cores (lower tier)
    //           PCPU* = performance cores (top tier)
    //   M5:     Apple renamed ECPU → MCPU in IOReport and introduced a third core tier.
    //           Three-tier architecture (sysctl hw.perflevel{N}.name):
    //             perflevel0 = Super       (top tier,    ex-P, PCPU* in IOReport)
    //             perflevel1 = Performance (mid tier,    Pro/Max only, MCPU* in IOReport)
    //             perflevel2 = Efficiency  (base M5 only, absent on Pro/Max)
    //           M5 Max example: 6 Super + 12 Performance + 0 Efficiency = 18 total.
    //   Ultra:  Any-generation Ultra chips prefix channels with "DIE_N_"
    //           (e.g. "DIE_0_ECPU0"), so use contains() not starts_with() — same
    //           pattern as Energy Model's "DIE_{}_CPU Energy".

    let cpu_domains = self.soc.cpu_domains.clone();
    let mut cpu_domain_cores = vec![Vec::new(); cpu_domains.len()];
    let mut rs = Metrics::default();

    for x in self.ior.get_sample() {
      // Keep this channel handling in sync with ioreport_channels_filter.
      if x.group == "CPU Stats" && x.subgroup == CPU_FREQ_CORE_SUBG {
        if let Some(domain_idx) = cpu_channel_domain_index(&x.channel, &cpu_domains) {
          let domain = &cpu_domains[domain_idx];
          let (freq_mhz, usage) = calc_freq(x.item, &domain.freqs_mhz);
          cpu_domain_cores[domain_idx].push(CoreUsageEntry { freq_mhz, usage });
          continue;
        }
      }

      if x.group == "GPU Stats" && x.subgroup == GPU_FREQ_DICE_SUBG {
        match x.channel.as_str() {
          "GPUPH" => {
            let (freq_mhz, usage) = calc_freq(x.item, &self.soc.gpu_freqs[1..]);
            rs.gpu_usage.push(GpuUsageEntry {
              name: x.channel.clone(),
              units: self.soc.gpu_cores as u32,
              freq_mhz,
              usage,
            });
          }
          _ => {}
        }
      }

      if x.group == "Energy Model" {
        match x.channel.as_str() {
          "GPU Energy" => rs.power.gpu += x.watts()?,
          // "CPU Energy" for Basic / Max, "DIE_{}_CPU Energy" for Ultra
          c if c.ends_with("CPU Energy") => rs.power.cpu += x.watts()?,
          // same pattern next keys: "ANE" for Basic, "ANE0" for Max, "ANE0_{}" for Ultra
          c if c.starts_with("ANE") => rs.power.ane += x.watts()?,
          c if c.starts_with("DRAM") => rs.power.ram += x.watts()?,
          c if c.starts_with("GPU SRAM") => rs.power.gpu_ram += x.watts()?,
          _ => {}
        }
      }
    }

    for (domain_idx, domain) in cpu_domains.iter().enumerate() {
      let cores = &cpu_domain_cores[domain_idx];
      if cores.is_empty() {
        continue;
      }

      let (freq_mhz, usage) = calc_cluster_usage_at_peak_freq(cores);
      rs.cpu_usage.push(CpuUsageEntry {
        name: domain.name.clone(),
        units: domain.units,
        freq_mhz,
        usage,
        cores: cores.clone(),
      });
    }

    rs.power.package = rs.power.cpu + rs.power.gpu + rs.power.ane + rs.power.ram + rs.power.gpu_ram;

    rs.memory = self.get_mem()?;
    rs.temp = self.get_temp()?;

    rs.power.board = self.smc.read_float_val("PSTR").unwrap_or(0.0);
    rs.power.battery = self.smc.read_float_val("PPBR").unwrap_or(0.0);
    rs.power.dc_in = self.smc.read_float_val("PDTR").unwrap_or(0.0);

    Ok(rs)
  }
}

#[cfg(test)]
mod tests {
  use super::{
    CoreUsageEntry, calc_cluster_usage_at_peak_freq, calc_freq_from_residencies,
    cpu_channel_domain_index,
  };
  use crate::sources::CpuDomainInfo;

  #[test]
  fn calc_freq_returns_raw_usage_ratio() {
    let items = vec![
      ("IDLE".to_string(), 50),
      ("F1".to_string(), 25),
      ("F2".to_string(), 15),
      ("F3".to_string(), 10),
    ];
    let (freq, usage) = calc_freq_from_residencies(&items, &[1000, 2000, 3000]);

    assert_eq!(freq, 1700);
    assert_eq!(usage, 0.5);
  }

  #[test]
  fn calc_freq_with_mismatched_states_matches_legacy_mapping() {
    let items = vec![
      ("IDLE".to_string(), 50),
      ("S1".to_string(), 0),
      ("S2".to_string(), 0),
      ("S3".to_string(), 0),
      ("S4".to_string(), 50),
    ];
    let (freq, usage) = calc_freq_from_residencies(&items, &[1000, 2000]);

    assert_eq!(freq, 0);
    assert!((usage - 0.5f32).abs() < 1e-6f32);
  }

  #[test]
  #[should_panic(expected = "calc_freq invalid data")]
  fn calc_freq_panics_when_frequency_table_outruns_active_states() {
    let items = vec![("IDLE".to_string(), 50), ("F1".to_string(), 50)];

    calc_freq_from_residencies(&items, &[1000, 2000]);
  }

  #[test]
  fn calc_cluster_usage_at_peak_freq_preserves_idle_frequency() {
    let cores = [
      CoreUsageEntry { freq_mhz: 0, usage: 0.0 },
      CoreUsageEntry { freq_mhz: 0, usage: 0.0 },
    ];
    let (freq, usage) = calc_cluster_usage_at_peak_freq(&cores);

    assert_eq!(freq, 0);
    assert_eq!(usage, 0.0);
  }

  #[test]
  fn calc_cluster_usage_at_peak_freq_scales_usage_to_peak_frequency() {
    let items = [
      CoreUsageEntry { freq_mhz: 4500, usage: 1.0 },
      CoreUsageEntry { freq_mhz: 1000, usage: 0.3 },
      CoreUsageEntry { freq_mhz: 0, usage: 0.0 },
      CoreUsageEntry { freq_mhz: 0, usage: 0.0 },
      CoreUsageEntry { freq_mhz: 0, usage: 0.0 },
      CoreUsageEntry { freq_mhz: 0, usage: 0.0 },
      CoreUsageEntry { freq_mhz: 0, usage: 0.0 },
      CoreUsageEntry { freq_mhz: 0, usage: 0.0 },
      CoreUsageEntry { freq_mhz: 0, usage: 0.0 },
      CoreUsageEntry { freq_mhz: 0, usage: 0.0 },
    ];
    let (freq, usage) = calc_cluster_usage_at_peak_freq(&items);

    assert_eq!(freq, 4500);
    assert!((usage - 0.10666).abs() < 1e-5);
  }

  #[test]
  fn ultra_cpu_channel_matching() {
    // On Ultra chips (M1/M2/M3 Ultra) IOReport CPU Stats channels are prefixed "DIE_N_".
    // These should be recognised; they were with contains() in v0.6.1 but broke when
    // ff5f058 changed to starts_with().
    let domains = vec![
      CpuDomainInfo { name: "ECPU".into(), units: 4, freqs_mhz: vec![1000] },
      CpuDomainInfo { name: "PCPU".into(), units: 8, freqs_mhz: vec![2000] },
      CpuDomainInfo { name: "MCPU".into(), units: 12, freqs_mhz: vec![1500] },
    ];
    let cases = [
      ("DIE_0_ECPU0", "ECPU"),
      ("DIE_1_ECPU0", "ECPU"),
      ("DIE_0_PCPU0", "PCPU"),
      ("DIE_1_PCPU0", "PCPU"),
      // Standard (non-Ultra) channels must still work
      ("ECPU0", "ECPU"),
      ("PCPU0", "PCPU"),
      ("MCPU0", "MCPU"),
    ];
    for (ch, expected) in cases {
      let matched = cpu_channel_domain_index(ch, &domains).map(|idx| domains[idx].name.as_str());
      assert_eq!(matched, Some(expected), "channel {ch}");
    }
  }
}
