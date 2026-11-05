#[divan::bench(sample_count = 20, sample_size = 1)]
fn subscription() {
  divan::black_box_drop(macmon::bench::init_ioreport().unwrap());
}

#[divan::bench(sample_count = 100, sample_size = 1)]
fn get_sample(bencher: divan::Bencher) {
  let mut ior = macmon::bench::init_ioreport().unwrap();
  bencher.bench_local(|| divan::black_box_drop(macmon::bench::get_sample(&mut ior)));
}
