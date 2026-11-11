//! Host telemetry for the UI's side panels. A background task samples the
//! machine every two seconds; `/api/status` returns the latest sample plus
//! live agent counters. Read-only.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::Serialize;
use sysinfo::{Disks, Gpus, ProcessesToUpdate, System};

const SAMPLE_EVERY: Duration = Duration::from_secs(2);
const TOP_PROCESSES: usize = 6;

#[derive(Debug, Clone, Default, Serialize)]
pub struct HostSnapshot {
    pub hostname: String,
    pub os: String,
    pub uptime_s: u64,
    pub load1: f64,
    pub cpu_pct: f32,
    pub cores: Vec<f32>,
    pub mem_used: u64,
    pub mem_total: u64,
    pub disk_used: u64,
    pub disk_total: u64,
    pub gpus: Vec<GpuSnapshot>,
    pub procs: Vec<ProcSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GpuSnapshot {
    pub name: String,
    pub usage_pct: Option<f32>,
    pub mem_used: Option<u64>,
    pub mem_total: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProcSnapshot {
    pub name: String,
    pub cpu_pct: f32,
    pub mem: u64,
}

pub type SharedSnapshot = Arc<RwLock<HostSnapshot>>;

struct Sampler {
    system: System,
    disks: Option<Disks>,
    gpus: Option<Gpus>,
}

impl Sampler {
    fn new() -> Option<Self> {
        Some(Self {
            system: System::new().ok()?,
            disks: Disks::new_with_refreshed_list().ok(),
            gpus: Gpus::new_with_refreshed_list().ok(),
        })
    }

    fn sample(&mut self) -> HostSnapshot {
        self.system.refresh_cpu_usage();
        self.system.refresh_memory();
        self.system.refresh_processes(ProcessesToUpdate::All, true);

        let cores: Vec<f32> = self.system.cpus().iter().map(|c| c.usage()).collect();
        let core_count = cores.len().max(1) as f32;

        // Report the largest mounted filesystem.
        let (disk_used, disk_total) = self
            .disks
            .as_mut()
            .map(|disks| {
                disks.refresh(true);
                disks
                    .iter()
                    .map(|d| (d.total_space() - d.available_space(), d.total_space()))
                    .max_by_key(|(_, total)| *total)
                    .unwrap_or((0, 0))
            })
            .unwrap_or((0, 0));

        let gpus = self
            .gpus
            .as_mut()
            .map(|gpus| {
                gpus.refresh(true);
                gpus.iter()
                    .map(|g| GpuSnapshot {
                        name: g.model().unwrap_or("GPU").to_string(),
                        usage_pct: g.usage(),
                        mem_used: g.used_memory(),
                        mem_total: g.total_memory(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        let mut procs: Vec<ProcSnapshot> = self
            .system
            .processes()
            .values()
            .map(|p| ProcSnapshot {
                name: p.name().to_string_lossy().into_owned(),
                // Normalised so 100 means the whole machine, not one core.
                cpu_pct: p.cpu_usage() / core_count,
                mem: p.memory(),
            })
            .collect();
        procs.sort_by(|a, b| {
            b.cpu_pct
                .total_cmp(&a.cpu_pct)
                .then_with(|| b.mem.cmp(&a.mem))
        });
        procs.truncate(TOP_PROCESSES);

        HostSnapshot {
            hostname: System::host_name().unwrap_or_default(),
            os: System::long_os_version().unwrap_or_default(),
            uptime_s: System::uptime().unwrap_or(0),
            load1: System::load_average().map(|l| l.one).unwrap_or(0.0),
            cpu_pct: self.system.global_cpu_usage(),
            cores,
            mem_used: self.system.used_memory(),
            mem_total: self.system.total_memory(),
            disk_used,
            disk_total,
            gpus,
            procs,
        }
    }
}

/// Start sampling on a blocking thread; the returned handle always holds
/// the most recent sample.
pub fn spawn_sampler(shared: SharedSnapshot) {
    std::thread::Builder::new()
        .name("host-sampler".into())
        .spawn(move || {
            let Some(mut sampler) = Sampler::new() else {
                tracing::warn!("host telemetry is unavailable on this platform");
                return;
            };
            loop {
                let snapshot = sampler.sample();
                *shared.write().unwrap() = snapshot;
                std::thread::sleep(SAMPLE_EVERY);
            }
        })
        .ok();
}
