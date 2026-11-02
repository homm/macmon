#ifndef MACMON_H
#define MACMON_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum macmon_status_t {
  MACMON_STATUS_OK = 0,
  MACMON_STATUS_INVALID_ARGUMENT = 1,
  MACMON_STATUS_INIT_FAILED = 2,
  MACMON_STATUS_SAMPLE_FAILED = 3,
  MACMON_STATUS_PANIC = 4
} macmon_status_t;

typedef struct macmon_sampler macmon_sampler_t;

typedef struct macmon_cpu_core_t {
  size_t die_id;
  size_t core_id;
  uint32_t freq_mhz;
  float scaled_ratio;
  float active_ratio;
} macmon_cpu_core_t;

typedef struct macmon_cpu_tier_t {
  const char *label;
  uint32_t freq_mhz;
  float scaled_ratio;
  float active_ratio;
  size_t cores_len;
  macmon_cpu_core_t *cores;
} macmon_cpu_tier_t;

typedef struct macmon_fan_t {
  const char *name;
  uint32_t rpm;
  uint32_t max_rpm; /* Zero when unavailable. */
} macmon_fan_t;

typedef struct macmon_mem_metrics_t {
  uint64_t ram_total;
  uint64_t ram_usage;
  uint64_t swap_total;
  uint64_t swap_usage;
} macmon_mem_metrics_t;

typedef struct macmon_temp_metrics_t {
  float cpu_temp_avg; /* Zero when unavailable. */
  float gpu_temp_avg; /* Zero when unavailable. */
} macmon_temp_metrics_t;

/* Ratios are 0..1, power is Watts, memory is bytes, temperatures are Celsius. */
typedef struct macmon_metrics_t {
  macmon_temp_metrics_t temp;
  macmon_mem_metrics_t memory;
  size_t fans_len;
  macmon_fan_t *fans;
  float cpu_scaled_ratio;
  float cpu_active_ratio;
  size_t cpu_tiers_len;
  macmon_cpu_tier_t *cpu_tiers;
  uint32_t gpu_freq_mhz;
  float gpu_scaled_ratio;
  float gpu_active_ratio;
  float cpu_power;
  float gpu_power;
  float ane_power;
  float chip_power; /* CLPC aggregate, or CPU + GPU + ANE + RAM + media. */
  float sys_power;
  float ram_power; /* DRAM + DCS. */
  float gpu_ram_power;
  float media_power; /* AVE + VDEC + ISP; missing reports contribute zero. */
  /* SMC readings; unavailable sensors return zero. These are not part of chip_power. */
  float display_power;
  float battery_power;
  float dc_in_power;
} macmon_metrics_t;

typedef struct macmon_cpu_tier_info_t {
  const char *label;
  uint8_t cores;
  size_t freqs_len;
  uint32_t *freqs;
} macmon_cpu_tier_info_t;

typedef struct macmon_soc_info_t {
  const char *mac_model;
  const char *chip_name;
  uint16_t memory_gb;
  size_t cpu_tiers_len;
  macmon_cpu_tier_info_t *cpu_tiers;
  uint8_t gpu_cores;
  size_t gpu_freqs_len;
  uint32_t *gpu_freqs;
  bool vm;
} macmon_soc_info_t;

/* Output structs own their nested arrays/strings; free them before reuse.
 * Free functions accept NULL and zero a supplied struct after releasing it.
 * Create, sample, and destroy each sampler on its owning thread; do not share it
 * concurrently. Sampling does not wait: callers control the interval, with the
 * first delta starting at sampler creation and later deltas at the prior call.
 * Hardware reads remain synchronous. */
macmon_status_t macmon_get_soc_info(macmon_soc_info_t *out_info);
void macmon_soc_info_free(macmon_soc_info_t *info);
macmon_status_t macmon_sampler_new(macmon_sampler_t **out_sampler);
void macmon_sampler_free(macmon_sampler_t *sampler);
macmon_status_t macmon_sampler_get_metrics(macmon_sampler_t *sampler, macmon_metrics_t *out_metrics);
void macmon_metrics_free(macmon_metrics_t *metrics);

/* Borrowed thread-local string, valid until the next API error/status call on
 * this thread. NULL means no error. Panics are reported when unwinding is enabled;
 * the normal release profile aborts on panic. */
const char *macmon_last_error_message(void);

#ifdef __cplusplus
}
#endif

#endif
