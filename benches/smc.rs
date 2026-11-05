#[divan::bench(sample_count = 20, sample_size = 1)]
fn init_full() {
  divan::black_box_drop(macmon::SmcSampler::new(Some(std::time::Duration::ZERO)).unwrap());
}

#[divan::bench(sample_count = 100, sample_size = 1)]
fn init_read_keys(bencher: divan::Bencher) {
  let mut smc = macmon::sources::SMC::new().unwrap();
  bencher.bench_local(|| divan::black_box_drop(smc.read_all_keys().unwrap()));
}

#[divan::bench(sample_count = 20, sample_size = 1)]
fn get_sample(bencher: divan::Bencher) {
  let mut sampler = macmon::SmcSampler::new(Some(std::time::Duration::ZERO)).unwrap();
  // Warm the key metadata cache before measuring steady-state reads.
  divan::black_box_drop(sampler.get_metrics().unwrap());
  bencher.bench_local(|| divan::black_box_drop(sampler.get_metrics().unwrap()));
}

#[divan::bench(sample_count = 20, sample_size = 1)]
fn get_temp_smc(bencher: divan::Bencher) {
  let mut sampler = macmon::SmcSampler::new(Some(std::time::Duration::ZERO)).unwrap();
  bencher.bench_local(|| divan::black_box_drop(sampler.get_temp_smc().unwrap()));
}
