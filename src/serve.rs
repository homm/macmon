use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, RwLock};
use std::thread;

use macmon::{Metrics, SocInfo};

pub type SharedMetrics = Arc<RwLock<Option<Metrics>>>;

#[rustfmt::skip]
fn to_prometheus(m: &Metrics, soc: &SocInfo) -> String {
  let chip = &soc.chip_name;
  let l = format!(r#"chip="{chip}""#);

  macro_rules! gauge {
    ($out:expr, $name:literal, $help:literal, $value:expr) => {
      gauge_labels!($out, $name, $help, &l, $value);
    };
  }

  macro_rules! gauge_labels {
    ($out:expr, $name:literal, $help:literal, $labels:expr, $value:expr) => {
      $out.push_str(&format!(
        "# HELP {} {}\n# TYPE {} gauge\n{}{{{l}}} {}\n\n",
        $name, $help, $name, $name, $value, l = $labels
      ));
    };
  }

  let mut out = String::new();
  gauge!(out, "macmon_cpu_temp_celsius", "Average CPU temperature in Celsius", m.temp.cpu_avg);
  gauge!(out, "macmon_gpu_temp_celsius", "Average GPU temperature in Celsius", m.temp.gpu_avg);
  gauge!(out, "macmon_memory_ram_total_bytes", "Total RAM in bytes", m.memory.ram_total);
  gauge!(out, "macmon_memory_ram_used_bytes", "Used RAM in bytes", m.memory.ram_usage);
  gauge!(out, "macmon_memory_swap_total_bytes", "Total swap in bytes", m.memory.swap_total);
  gauge!(out, "macmon_memory_swap_used_bytes", "Used swap in bytes", m.memory.swap_usage);
  for domain in &m.cpu_usage {
    let labels = format!(r#"chip="{chip}",domain="{}""#, domain.name);
    gauge_labels!(out, "macmon_cpu_usage_freq_mhz", "CPU domain frequency in MHz", &labels, domain.freq_mhz);
    gauge_labels!(out, "macmon_cpu_usage_ratio", "CPU domain utilization (0–1)", &labels, domain.usage);
  }
  for domain in &m.gpu_usage {
    let labels = format!(r#"chip="{chip}",domain="{}""#, domain.name);
    gauge_labels!(out, "macmon_gpu_usage_freq_mhz", "GPU domain frequency in MHz", &labels, domain.freq_mhz);
    gauge_labels!(out, "macmon_gpu_usage_ratio", "GPU domain utilization (0–1)", &labels, domain.usage);
  }
  gauge!(out, "macmon_power_package_watts", "SoC/package power consumption in Watts", m.power.package);
  gauge!(out, "macmon_power_cpu_watts", "CPU power consumption in Watts", m.power.cpu);
  gauge!(out, "macmon_power_gpu_watts", "GPU power consumption in Watts", m.power.gpu);
  gauge!(out, "macmon_power_ram_watts", "RAM power consumption in Watts", m.power.ram);
  gauge!(out, "macmon_power_gpu_ram_watts", "GPU RAM power consumption in Watts", m.power.gpu_ram);
  gauge!(out, "macmon_power_ane_watts", "Apple Neural Engine power consumption in Watts", m.power.ane);
  gauge!(out, "macmon_power_board_watts", "Total system power consumption in Watts", m.power.board);
  gauge!(out, "macmon_power_battery_watts", "Battery rail power in Watts", m.power.battery);
  gauge!(out, "macmon_power_dc_in_watts", "External DC input power in Watts", m.power.dc_in);
  out
}

fn to_json(m: &Metrics, soc: &SocInfo) -> String {
  let mut doc = serde_json::to_value(m).unwrap_or_default();
  doc["soc"] = serde_json::to_value(soc).unwrap_or_default();
  doc["timestamp"] = serde_json::to_value(chrono::Utc::now().to_rfc3339()).unwrap_or_default();
  serde_json::to_string(&doc).unwrap_or_default()
}

fn read_path(stream: &mut TcpStream) -> Option<String> {
  let mut buf = [0u8; 2048];
  let n = stream.read(&mut buf).ok()?;
  let text = std::str::from_utf8(&buf[..n]).ok()?;
  let path = text.lines().next()?.split_whitespace().nth(1)?;
  Some(path.split('?').next().unwrap_or(path).to_string())
}

fn write_response(stream: &mut TcpStream, status: u16, content_type: &str, body: String) {
  let status_text = match status {
    200 => "OK",
    404 => "Not Found",
    503 => "Service Unavailable",
    _ => "OK",
  };
  let _ = stream.write_all(
    format!(
      "HTTP/1.1 {status} {status_text}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
      body.len()
    )
    .as_bytes(),
  );
}

fn handle_conn(mut stream: TcpStream, shared: SharedMetrics, soc: Arc<SocInfo>) {
  let path = match read_path(&mut stream) {
    Some(p) => p,
    None => return,
  };

  if path == "/" {
    write_response(&mut stream, 200, "application/json", r#"{}"#.to_string());
    return;
  }

  let lock = shared.read().unwrap();

  let Some(m) = lock.as_ref() else {
    drop(lock);
    write_response(&mut stream, 503, "application/json", r#"{"error":"no data yet"}"#.to_string());
    return;
  };

  match path.as_str() {
    "/json" => {
      let body = to_json(m, &soc);
      drop(lock);
      write_response(&mut stream, 200, "application/json", body);
    }
    "/metrics" => {
      let body = to_prometheus(m, &soc);
      drop(lock);
      write_response(&mut stream, 200, "text/plain; version=0.0.4; charset=utf-8", body);
    }
    _ => {
      drop(lock);
      write_response(&mut stream, 404, "application/json", r#"{"error":"not found"}"#.to_string());
    }
  }
}

pub fn launchd(port: u16, install: bool) -> Result<(), Box<dyn std::error::Error>> {
  let home = std::env::var("HOME")?;
  let plist_path = format!("{home}/Library/LaunchAgents/com.macmon.plist");

  if !install {
    let _ = std::process::Command::new("launchctl")
      .args(["unload", &plist_path])
      .stdout(std::process::Stdio::null())
      .stderr(std::process::Stdio::null())
      .status();
    std::fs::remove_file(&plist_path)?;
    eprintln!("macmon service uninstalled");
    return Ok(());
  }

  let bin = std::env::current_exe()?;
  let bin = bin.to_string_lossy();
  let plist = format!(
    r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.macmon</string>
  <key>ProgramArguments</key>
  <array>
    <string>{bin}</string>
    <string>serve</string>
    <string>--port</string>
    <string>{port}</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
</dict>
</plist>
"#
  );

  let agents_dir = format!("{home}/Library/LaunchAgents");
  std::fs::create_dir_all(&agents_dir)?;

  // unload silently in case it's already running
  let _ = std::process::Command::new("launchctl")
    .args(["unload", &plist_path])
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::null())
    .status();

  std::fs::write(&plist_path, plist)?;
  std::process::Command::new("launchctl").args(["load", &plist_path]).status()?;
  eprintln!("macmon service installed: {plist_path}");
  eprintln!("serving on http://localhost:{port}");

  Ok(())
}

pub fn run(
  port: u16,
  shared: SharedMetrics,
  soc: Arc<SocInfo>,
) -> Result<(), Box<dyn std::error::Error>> {
  let listener = TcpListener::bind(format!("0.0.0.0:{port}"))?;
  eprintln!("macmon serving on http://localhost:{port}");
  eprintln!("  GET /json    → JSON metrics");
  eprintln!("  GET /metrics → Prometheus format");

  for stream in listener.incoming() {
    let Ok(stream) = stream else { continue };
    let shared = Arc::clone(&shared);
    let soc = Arc::clone(&soc);
    thread::spawn(move || handle_conn(stream, shared, soc));
  }

  Ok(())
}
