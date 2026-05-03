use core_foundation::dictionary::CFDictionaryRef;
use serde::Serialize;

use crate::sources::{
  IOHIDSensors, IOReport, SMC, SocInfo, cfio_get_residencies, get_soc_info, libc_ram, libc_swap,
};

type WithError<T> = Result<T, Box<dyn std::error::Error>>;

// const CPU_FREQ_DICE_SUBG: &str = "CPU Complex Performance States";
const CPU_FREQ_CORE_SUBG: &str = "CPU Core Performance States";
const GPU_FREQ_DICE_SUBG: &str = "GPU Performance States";

// MARK: Structs

#[derive(Debug, Default, Serialize)]
pub struct TempMetrics {
  pub cpu_temp_avg: f32, // Celsius
  pub gpu_temp_avg: f32, // Celsius
}

#[derive(Debug, Default, Serialize)]
pub struct MemMetrics {
  pub ram_total: u64,  // bytes
  pub ram_usage: u64,  // bytes
  pub swap_total: u64, // bytes
  pub swap_usage: u64, // bytes
}

#[derive(Debug, Default, Serialize)]
pub struct Metrics {
  pub temp: TempMetrics,
  pub memory: MemMetrics,
  pub ecpu_usage: (u32, f32), // freq MHz, usage ratio
  pub pcpu_usage: (u32, f32), // freq MHz, usage ratio
  pub cpu_usage_pct: f32,     // combined ecpu+pcpu usage, weighted by core count
  pub gpu_usage: (u32, f32),  // freq MHz, usage ratio
  pub cpu_power: f32,         // Watts
  pub gpu_power: f32,         // Watts
  pub ane_power: f32,         // Watts
  pub all_power: f32,         // Watts
  pub sys_power: f32,         // Watts
  pub ram_power: f32,         // Watts
  pub gpu_ram_power: f32,     // Watts
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

fn calc_cluster_usage_at_peak_freq(items: &[(u32, f32)]) -> (u32, f32) {
  let peak_freq = items.iter().filter(|x| x.1 > 0.0).map(|x| x.0).max().unwrap_or(0);
  if peak_freq == 0 {
    return (0, 0.0);
  }

  let peak_freq = peak_freq as f32;
  let usage =
    zero_div(items.iter().map(|x| x.1 * x.0 as f32 / peak_freq).sum(), items.len() as f32);

  (peak_freq as u32, usage)
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

    let cpu_temp_avg = zero_div(cpu_metrics.iter().sum::<f32>(), cpu_metrics.len() as f32);
    let gpu_temp_avg = zero_div(gpu_metrics.iter().sum::<f32>(), gpu_metrics.len() as f32);

    Ok(TempMetrics { cpu_temp_avg, gpu_temp_avg })
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

    let cpu_temp_avg = zero_div(cpu_values.iter().sum(), cpu_values.len() as f32);
    let gpu_temp_avg = zero_div(gpu_values.iter().sum(), gpu_values.len() as f32);

    Ok(TempMetrics { cpu_temp_avg, gpu_temp_avg })
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

  fn get_sys_power(&mut self) -> WithError<f32> {
    self.smc.read_float_val("PSTR")
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

    let mut ecpu_usages = Vec::new();
    let mut pcpu_usages = Vec::new();
    let mut rs = Metrics::default();

    for x in self.ior.get_sample() {
      // Keep this channel handling in sync with ioreport_channels_filter.
      if x.group == "CPU Stats" && x.subgroup == CPU_FREQ_CORE_SUBG {
        if x.channel.contains("PCPU") {
          pcpu_usages.push(calc_freq(x.item, &self.soc.pcpu_freqs));
          continue;
        }

        if x.channel.contains("ECPU") || x.channel.contains("MCPU") {
          ecpu_usages.push(calc_freq(x.item, &self.soc.ecpu_freqs));
          continue;
        }
      }

      if x.group == "GPU Stats" && x.subgroup == GPU_FREQ_DICE_SUBG {
        match x.channel.as_str() {
          "GPUPH" => rs.gpu_usage = calc_freq(x.item, &self.soc.gpu_freqs[1..]),
          _ => {}
        }
      }

      if x.group == "Energy Model" {
        match x.channel.as_str() {
          "GPU Energy" => rs.gpu_power += x.watts()?,
          // "CPU Energy" for Basic / Max, "DIE_{}_CPU Energy" for Ultra
          c if c.ends_with("CPU Energy") => rs.cpu_power += x.watts()?,
          // same pattern next keys: "ANE" for Basic, "ANE0" for Max, "ANE0_{}" for Ultra
          c if c.starts_with("ANE") => rs.ane_power += x.watts()?,
          c if c.starts_with("DRAM") => rs.ram_power += x.watts()?,
          c if c.starts_with("GPU SRAM") => rs.gpu_ram_power += x.watts()?,
          _ => {}
        }
      }
    }

    // Filter dead/disabled cores (e.g. M5 Max MCPU0 cluster is all-DOWN)
    ecpu_usages.retain(|&(_, pct)| pct > 0.0);
    rs.ecpu_usage = calc_cluster_usage_at_peak_freq(&ecpu_usages);
    rs.pcpu_usage = calc_cluster_usage_at_peak_freq(&pcpu_usages);

    let ecores = self.soc.ecpu_cores as f32;
    let pcores = self.soc.pcpu_cores as f32;
    let tcores = ecores + pcores;

    rs.cpu_usage_pct = zero_div(rs.ecpu_usage.1 * ecores + rs.pcpu_usage.1 * pcores, tcores);
    rs.all_power = rs.cpu_power + rs.gpu_power + rs.ane_power;

    rs.memory = self.get_mem()?;
    rs.temp = self.get_temp()?;

    rs.sys_power = match self.get_sys_power() {
      Ok(val) => val.max(rs.all_power),
      Err(_) => 0.0,
    };

    Ok(rs)
  }
}

#[cfg(test)]
mod tests {
  use super::{calc_cluster_usage_at_peak_freq, calc_freq_from_residencies};

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
    let (freq, usage) = calc_cluster_usage_at_peak_freq(&[(0, 0.0), (0, 0.0)]);

    assert_eq!(freq, 0);
    assert_eq!(usage, 0.0);
  }

  #[test]
  fn calc_cluster_usage_at_peak_freq_scales_usage_to_peak_frequency() {
    let items = [
      (4500, 1.0),
      (1000, 0.3),
      (0, 0.0),
      (0, 0.0),
      (0, 0.0),
      (0, 0.0),
      (0, 0.0),
      (0, 0.0),
      (0, 0.0),
      (0, 0.0),
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
    let cases = [
      ("DIE_0_ECPU0", "ecpu"),
      ("DIE_1_ECPU0", "ecpu"),
      ("DIE_0_PCPU0", "pcpu"),
      ("DIE_1_PCPU0", "pcpu"),
      // Standard (non-Ultra) channels must still work
      ("ECPU0", "ecpu"),
      ("PCPU0", "pcpu"),
      ("MCPU0", "ecpu"), // M5+ performance cores map to ecpu slot
    ];
    for (ch, expected) in cases {
      let matched = if ch.contains("PCPU") {
        "pcpu"
      } else if ch.contains("ECPU") || ch.contains("MCPU") {
        "ecpu"
      } else {
        "none"
      };
      assert_eq!(matched, expected, "channel {ch}");
    }
  }
}
