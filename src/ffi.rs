//! Owned C snapshots and sampler handles. See `include/macmon.h` for the contract.

use std::cell::RefCell;
use std::ffi::{CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

use crate::metrics::{Metrics, Sampler};
use crate::sources::{SocInfo, get_soc_info};

thread_local! {
  static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum macmon_status_t {
  MACMON_STATUS_OK = 0,
  MACMON_STATUS_INVALID_ARGUMENT = 1,
  MACMON_STATUS_INIT_FAILED = 2,
  MACMON_STATUS_SAMPLE_FAILED = 3,
  MACMON_STATUS_PANIC = 4,
}

#[repr(C)]
pub struct macmon_sampler_t {
  _private: [u8; 0],
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_cpu_core_t {
  pub die_id: usize,
  pub core_id: usize,
  pub freq_mhz: u32,
  pub scaled_ratio: f32,
  pub active_ratio: f32,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_cpu_tier_t {
  pub label: *const c_char,
  pub freq_mhz: u32,
  pub scaled_ratio: f32,
  pub active_ratio: f32,
  pub cores_len: usize,
  pub cores: *mut macmon_cpu_core_t,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_fan_t {
  pub name: *const c_char,
  pub rpm: u32,
  pub max_rpm: u32,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_mem_metrics_t {
  pub ram_total: u64,
  pub ram_usage: u64,
  pub swap_total: u64,
  pub swap_usage: u64,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_temp_metrics_t {
  pub cpu_temp_avg: f32,
  pub gpu_temp_avg: f32,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_metrics_t {
  pub temp: macmon_temp_metrics_t,
  pub memory: macmon_mem_metrics_t,
  pub fans_len: usize,
  pub fans: *mut macmon_fan_t,
  pub cpu_scaled_ratio: f32,
  pub cpu_active_ratio: f32,
  pub cpu_tiers_len: usize,
  pub cpu_tiers: *mut macmon_cpu_tier_t,
  pub gpu_freq_mhz: u32,
  pub gpu_scaled_ratio: f32,
  pub gpu_active_ratio: f32,
  pub cpu_power: f32,
  pub gpu_power: f32,
  pub ane_power: f32,
  pub chip_power: f32,
  pub sys_power: f32,
  pub ram_power: f32,
  pub gpu_ram_power: f32,
  pub media_power: f32,
  pub display_power: f32,
  pub battery_power: f32,
  pub dc_in_power: f32,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_cpu_tier_info_t {
  pub label: *const c_char,
  pub cores: u8,
  pub freqs_len: usize,
  pub freqs: *mut u32,
}

#[repr(C)]
#[derive(Debug, Default)]
pub struct macmon_soc_info_t {
  pub mac_model: *const c_char,
  pub chip_name: *const c_char,
  pub memory_gb: u16,
  pub cpu_tiers_len: usize,
  pub cpu_tiers: *mut macmon_cpu_tier_info_t,
  pub gpu_cores: u8,
  pub gpu_freqs_len: usize,
  pub gpu_freqs: *mut u32,
  pub vm: bool,
}

struct FfiError {
  status: macmon_status_t,
  message: String,
}

fn ffi_string(value: &str) -> *const c_char {
  CString::new(value.replace('\0', " ")).unwrap().into_raw()
}

fn set_last_error(message: &str) {
  LAST_ERROR.with(|slot| {
    *slot.borrow_mut() = Some(CString::new(message.replace('\0', " ")).unwrap());
  });
}

fn ffi_status(f: impl FnOnce() -> Result<(), FfiError>) -> macmon_status_t {
  match catch_unwind(AssertUnwindSafe(f)) {
    Ok(Ok(())) => {
      LAST_ERROR.with(|slot| *slot.borrow_mut() = None);
      macmon_status_t::MACMON_STATUS_OK
    }
    Ok(Err(error)) => {
      set_last_error(&error.message);
      error.status
    }
    Err(_) => {
      set_last_error("panic across FFI boundary");
      macmon_status_t::MACMON_STATUS_PANIC
    }
  }
}

fn invalid_argument(message: &str) -> FfiError {
  FfiError { status: macmon_status_t::MACMON_STATUS_INVALID_ARGUMENT, message: message.into() }
}

fn ffi_array<T>(values: Vec<T>) -> (*mut T, usize) {
  let values = values.into_boxed_slice();
  let len = values.len();
  (Box::into_raw(values) as *mut T, len)
}

unsafe fn free_array<T>(values: *mut T, len: usize) {
  if !values.is_null() {
    unsafe { drop(Box::from_raw(ptr::slice_from_raw_parts_mut(values, len))) };
  }
}

unsafe fn free_string(value: *const c_char) {
  if !value.is_null() {
    unsafe { drop(CString::from_raw(value as *mut c_char)) };
  }
}

fn ffi_metrics(metrics: Metrics) -> macmon_metrics_t {
  let (cpu_tiers, cpu_tiers_len) = ffi_array(
    metrics
      .cpu_tiers
      .into_iter()
      .map(|tier| {
        let (cores, cores_len) = ffi_array(
          tier
            .cores
            .into_iter()
            .map(|core| macmon_cpu_core_t {
              die_id: core.die_id,
              core_id: core.core_id,
              freq_mhz: core.freq_mhz,
              scaled_ratio: core.scaled_ratio,
              active_ratio: core.active_ratio,
            })
            .collect(),
        );
        macmon_cpu_tier_t {
          label: ffi_string(&tier.label),
          freq_mhz: tier.freq_mhz,
          scaled_ratio: tier.scaled_ratio,
          active_ratio: tier.active_ratio,
          cores_len,
          cores,
        }
      })
      .collect(),
  );
  let (fans, fans_len) = ffi_array(
    metrics
      .fans
      .into_iter()
      .map(|fan| macmon_fan_t {
        name: ffi_string(&fan.name),
        rpm: fan.rpm,
        max_rpm: fan.max_rpm.unwrap_or_default(),
      })
      .collect(),
  );
  macmon_metrics_t {
    temp: macmon_temp_metrics_t {
      cpu_temp_avg: metrics.temp.cpu_temp_avg.unwrap_or_default(),
      gpu_temp_avg: metrics.temp.gpu_temp_avg.unwrap_or_default(),
    },
    memory: macmon_mem_metrics_t {
      ram_total: metrics.memory.ram_total,
      ram_usage: metrics.memory.ram_usage,
      swap_total: metrics.memory.swap_total,
      swap_usage: metrics.memory.swap_usage,
    },
    fans_len,
    fans,
    cpu_scaled_ratio: metrics.cpu_scaled_ratio,
    cpu_active_ratio: metrics.cpu_active_ratio,
    cpu_tiers_len,
    cpu_tiers,
    gpu_freq_mhz: metrics.gpu_freq_mhz,
    gpu_scaled_ratio: metrics.gpu_scaled_ratio,
    gpu_active_ratio: metrics.gpu_active_ratio,
    cpu_power: metrics.cpu_power,
    gpu_power: metrics.gpu_power,
    ane_power: metrics.ane_power,
    chip_power: metrics.chip_power,
    sys_power: metrics.sys_power,
    ram_power: metrics.ram_power,
    gpu_ram_power: metrics.gpu_ram_power,
    media_power: metrics.media_power,
    display_power: metrics.display_power,
    battery_power: metrics.battery_power,
    dc_in_power: metrics.dc_in_power,
  }
}

fn ffi_soc_info(info: SocInfo) -> macmon_soc_info_t {
  let (cpu_tiers, cpu_tiers_len) = ffi_array(
    info
      .cpu_tiers
      .into_iter()
      .map(|tier| {
        let (freqs, freqs_len) = ffi_array(tier.freqs);
        macmon_cpu_tier_info_t {
          label: ffi_string(&tier.label),
          cores: tier.cores,
          freqs_len,
          freqs,
        }
      })
      .collect(),
  );
  let (gpu_freqs, gpu_freqs_len) = ffi_array(info.gpu_freqs);
  macmon_soc_info_t {
    mac_model: ffi_string(&info.mac_model),
    chip_name: ffi_string(&info.chip_name),
    memory_gb: info.memory_gb,
    cpu_tiers_len,
    cpu_tiers,
    gpu_cores: info.gpu_cores,
    gpu_freqs_len,
    gpu_freqs,
    vm: info.vm,
  }
}

#[unsafe(no_mangle)]
/// # Safety
/// `out_sampler` must point to a writable handle slot.
pub unsafe extern "C" fn macmon_sampler_new(
  out_sampler: *mut *mut macmon_sampler_t,
) -> macmon_status_t {
  ffi_status(|| {
    if out_sampler.is_null() {
      return Err(invalid_argument("out_sampler must not be null"));
    }
    unsafe { *out_sampler = ptr::null_mut() };
    let sampler = Sampler::new().map_err(|error| FfiError {
      status: macmon_status_t::MACMON_STATUS_INIT_FAILED,
      message: error.to_string(),
    })?;
    unsafe { *out_sampler = Box::into_raw(Box::new(sampler)) as *mut macmon_sampler_t };
    Ok(())
  })
}

#[unsafe(no_mangle)]
/// # Safety
/// `sampler` must be null or an unfreed handle returned by `macmon_sampler_new`.
/// It must be destroyed on its owning thread without concurrent access.
pub unsafe extern "C" fn macmon_sampler_free(sampler: *mut macmon_sampler_t) {
  let result = catch_unwind(AssertUnwindSafe(|| {
    if !sampler.is_null() {
      unsafe { drop(Box::from_raw(sampler as *mut Sampler)) };
    }
  }));
  if result.is_err() {
    set_last_error("panic across FFI boundary");
  }
}

#[unsafe(no_mangle)]
/// # Safety
/// `out_info` must point to a writable slot whose previous contents have been freed.
pub unsafe extern "C" fn macmon_get_soc_info(out_info: *mut macmon_soc_info_t) -> macmon_status_t {
  ffi_status(|| {
    if out_info.is_null() {
      return Err(invalid_argument("out_info must not be null"));
    }
    unsafe { ptr::write(out_info, macmon_soc_info_t::default()) };
    let info = get_soc_info().map_err(|error| FfiError {
      status: macmon_status_t::MACMON_STATUS_INIT_FAILED,
      message: error.to_string(),
    })?;
    unsafe { ptr::write(out_info, ffi_soc_info(info)) };
    Ok(())
  })
}

#[unsafe(no_mangle)]
/// # Safety
/// `info` must be null or point to an initialized struct returned by `macmon_get_soc_info`.
pub unsafe extern "C" fn macmon_soc_info_free(info: *mut macmon_soc_info_t) {
  let result = catch_unwind(AssertUnwindSafe(|| {
    if info.is_null() {
      return;
    }
    let info = unsafe { &mut *info };
    unsafe {
      free_string(info.mac_model);
      free_string(info.chip_name);
      if !info.cpu_tiers.is_null() {
        for tier in std::slice::from_raw_parts(info.cpu_tiers, info.cpu_tiers_len) {
          free_string(tier.label);
          free_array(tier.freqs, tier.freqs_len);
        }
      }
      free_array(info.cpu_tiers, info.cpu_tiers_len);
      free_array(info.gpu_freqs, info.gpu_freqs_len);
    }
    *info = macmon_soc_info_t::default();
  }));
  if result.is_err() {
    set_last_error("panic across FFI boundary");
  }
}

#[unsafe(no_mangle)]
/// # Safety
/// `sampler` must be an unfreed handle used exclusively on its owning thread.
/// `out_metrics` must point to a writable slot whose previous contents have been freed.
pub unsafe extern "C" fn macmon_sampler_get_metrics(
  sampler: *mut macmon_sampler_t,
  out_metrics: *mut macmon_metrics_t,
) -> macmon_status_t {
  ffi_status(|| {
    if out_metrics.is_null() {
      return Err(invalid_argument("out_metrics must not be null"));
    }
    unsafe { ptr::write(out_metrics, macmon_metrics_t::default()) };
    if sampler.is_null() {
      return Err(invalid_argument("sampler must not be null"));
    }
    let sampler = unsafe { &mut *(sampler as *mut Sampler) };
    let metrics = sampler.get_metrics().map_err(|error| FfiError {
      status: macmon_status_t::MACMON_STATUS_SAMPLE_FAILED,
      message: error.to_string(),
    })?;
    unsafe { ptr::write(out_metrics, ffi_metrics(metrics)) };
    Ok(())
  })
}

#[unsafe(no_mangle)]
/// # Safety
/// `metrics` must be null or point to an initialized struct returned by
/// `macmon_sampler_get_metrics`.
pub unsafe extern "C" fn macmon_metrics_free(metrics: *mut macmon_metrics_t) {
  let result = catch_unwind(AssertUnwindSafe(|| {
    if metrics.is_null() {
      return;
    }
    let metrics = unsafe { &mut *metrics };
    unsafe {
      if !metrics.cpu_tiers.is_null() {
        for tier in std::slice::from_raw_parts(metrics.cpu_tiers, metrics.cpu_tiers_len) {
          free_string(tier.label);
          free_array(tier.cores, tier.cores_len);
        }
      }
      free_array(metrics.cpu_tiers, metrics.cpu_tiers_len);
      if !metrics.fans.is_null() {
        for fan in std::slice::from_raw_parts(metrics.fans, metrics.fans_len) {
          free_string(fan.name);
        }
      }
      free_array(metrics.fans, metrics.fans_len);
    }
    *metrics = macmon_metrics_t::default();
  }));
  if result.is_err() {
    set_last_error("panic across FFI boundary");
  }
}

#[unsafe(no_mangle)]
pub extern "C" fn macmon_last_error_message() -> *const c_char {
  LAST_ERROR.with(|slot| slot.borrow().as_ref().map_or(ptr::null(), |message| message.as_ptr()))
}

#[cfg(test)]
mod tests {
  use std::ffi::CStr;

  use super::*;
  use crate::metrics::{CpuCoreMetrics, CpuTierMetrics, FanMetric, MemMetrics, TempMetrics};
  use crate::sources::CpuTierInfo;

  #[test]
  fn snapshots_preserve_metrics_and_ownership_and_default_missing_values_to_zero() {
    let mut raw = ffi_metrics(Metrics {
      temp: TempMetrics { cpu_temp_avg: Some(42.0), gpu_temp_avg: None },
      memory: MemMetrics { ram_total: 10, ram_usage: 5, swap_total: 2, swap_usage: 1 },
      fans: vec![
        FanMetric { name: "fan0".into(), rpm: 900, max_rpm: Some(3000) },
        FanMetric { name: "fan1".into(), rpm: 0, max_rpm: None },
      ],
      cpu_scaled_ratio: 0.25,
      cpu_active_ratio: 0.5,
      cpu_tiers: vec![CpuTierMetrics {
        label: "S".into(),
        freq_mhz: 1200,
        scaled_ratio: 0.2,
        active_ratio: 0.4,
        cores: vec![CpuCoreMetrics {
          die_id: 1,
          core_id: 3,
          freq_mhz: 1100,
          scaled_ratio: 0.15,
          active_ratio: 0.3,
        }],
      }],
      gpu_freq_mhz: 500,
      gpu_scaled_ratio: 0.1,
      gpu_active_ratio: 0.2,
      cpu_power: 1.0,
      gpu_power: 2.0,
      ane_power: 3.0,
      chip_power: 15.0,
      sys_power: 12.0,
      ram_power: 4.0,
      gpu_ram_power: 5.0,
      media_power: 6.0,
      display_power: 7.0,
      battery_power: 8.0,
      dc_in_power: 9.0,
      ..Default::default()
    });
    assert_eq!(raw.temp.cpu_temp_avg, 42.0);
    assert_eq!(raw.temp.gpu_temp_avg, 0.0);
    assert_eq!(
      (raw.memory.ram_total, raw.memory.ram_usage, raw.memory.swap_total, raw.memory.swap_usage),
      (10, 5, 2, 1)
    );
    assert_eq!((raw.cpu_scaled_ratio, raw.cpu_active_ratio), (0.25, 0.5));
    assert_eq!(raw.cpu_tiers_len, 1);
    let tier = unsafe { &*raw.cpu_tiers };
    assert_eq!(unsafe { CStr::from_ptr(tier.label) }.to_str().unwrap(), "S");
    assert_eq!((tier.freq_mhz, tier.scaled_ratio, tier.active_ratio), (1200, 0.2, 0.4));
    assert_eq!(tier.cores_len, 1);
    let core = unsafe { &*tier.cores };
    assert_eq!(
      (core.die_id, core.core_id, core.freq_mhz, core.scaled_ratio, core.active_ratio),
      (1, 3, 1100, 0.15, 0.3)
    );
    assert_eq!(raw.fans_len, 2);
    let fans = unsafe { std::slice::from_raw_parts(raw.fans, raw.fans_len) };
    assert_eq!(unsafe { CStr::from_ptr(fans[0].name) }.to_str().unwrap(), "fan0");
    assert_eq!(fans[0].rpm, 900);
    assert_eq!(fans[0].max_rpm, 3000);
    assert_eq!(fans[1].max_rpm, 0);
    assert_eq!((raw.gpu_freq_mhz, raw.gpu_scaled_ratio, raw.gpu_active_ratio), (500, 0.1, 0.2));
    assert_eq!(
      (
        raw.cpu_power,
        raw.gpu_power,
        raw.ane_power,
        raw.chip_power,
        raw.sys_power,
        raw.ram_power,
        raw.gpu_ram_power,
        raw.media_power
      ),
      (1.0, 2.0, 3.0, 15.0, 12.0, 4.0, 5.0, 6.0)
    );
    assert_eq!((raw.display_power, raw.battery_power, raw.dc_in_power), (7.0, 8.0, 9.0));
    unsafe { macmon_metrics_free(&mut raw) };
    assert!(raw.cpu_tiers.is_null() && raw.fans.is_null());
    assert_eq!((raw.cpu_tiers_len, raw.fans_len), (0, 0));
    unsafe { macmon_metrics_free(&mut raw) };
    unsafe { macmon_metrics_free(ptr::null_mut()) };
  }

  #[test]
  fn soc_snapshot_preserves_tiers_frequency_tables_and_vm_flag() {
    let mut raw = ffi_soc_info(SocInfo {
      mac_model: "Mac99,1".into(),
      chip_name: "Apple M99".into(),
      memory_gb: 256,
      cpu_tiers: vec![CpuTierInfo { label: "P".into(), cores: 8, freqs: vec![1000, 2000] }],
      gpu_cores: 40,
      gpu_freqs: vec![300, 600],
      vm: true,
      ..Default::default()
    });
    assert_eq!(unsafe { CStr::from_ptr(raw.mac_model) }.to_str().unwrap(), "Mac99,1");
    assert_eq!(unsafe { CStr::from_ptr(raw.chip_name) }.to_str().unwrap(), "Apple M99");
    assert_eq!((raw.memory_gb, raw.gpu_cores, raw.vm), (256, 40, true));
    assert_eq!(raw.cpu_tiers_len, 1);
    let tier = unsafe { &*raw.cpu_tiers };
    assert_eq!(unsafe { CStr::from_ptr(tier.label) }.to_str().unwrap(), "P");
    assert_eq!(tier.cores, 8);
    assert_eq!(unsafe { std::slice::from_raw_parts(tier.freqs, tier.freqs_len) }, [1000, 2000]);
    assert_eq!(unsafe { std::slice::from_raw_parts(raw.gpu_freqs, raw.gpu_freqs_len) }, [300, 600]);
    unsafe { macmon_soc_info_free(&mut raw) };
    assert!(
      raw.mac_model.is_null()
        && raw.chip_name.is_null()
        && raw.cpu_tiers.is_null()
        && raw.gpu_freqs.is_null()
    );
    assert_eq!((raw.cpu_tiers_len, raw.gpu_freqs_len), (0, 0));
    unsafe { macmon_soc_info_free(&mut raw) };
    unsafe { macmon_soc_info_free(ptr::null_mut()) };
  }

  #[test]
  fn invalid_arguments_zero_outputs_and_errors_are_thread_local() {
    assert_eq!(
      unsafe { macmon_sampler_new(ptr::null_mut()) },
      macmon_status_t::MACMON_STATUS_INVALID_ARGUMENT
    );
    assert_eq!(
      unsafe { macmon_get_soc_info(ptr::null_mut()) },
      macmon_status_t::MACMON_STATUS_INVALID_ARGUMENT
    );
    assert_eq!(
      unsafe { macmon_sampler_get_metrics(ptr::null_mut(), ptr::null_mut()) },
      macmon_status_t::MACMON_STATUS_INVALID_ARGUMENT
    );
    let mut metrics = macmon_metrics_t { cpu_power: 9.0, ..Default::default() };
    assert_eq!(
      unsafe { macmon_sampler_get_metrics(ptr::null_mut(), &mut metrics) },
      macmon_status_t::MACMON_STATUS_INVALID_ARGUMENT
    );
    assert_eq!(metrics.cpu_power, 0.0);
    assert!(
      unsafe { CStr::from_ptr(macmon_last_error_message()) }.to_str().unwrap().contains("sampler")
    );
    std::thread::spawn(|| {
      assert!(macmon_last_error_message().is_null());
      set_last_error("other thread");
    })
    .join()
    .unwrap();
    assert!(
      unsafe { CStr::from_ptr(macmon_last_error_message()) }.to_str().unwrap().contains("sampler")
    );
    assert_eq!(ffi_status(|| Ok(())), macmon_status_t::MACMON_STATUS_OK);
    assert!(macmon_last_error_message().is_null());
    unsafe { macmon_sampler_free(ptr::null_mut()) };
  }
}
