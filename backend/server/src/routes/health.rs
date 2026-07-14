use axum::{http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;
use sysinfo::{get_current_pid, ProcessesToUpdate, System};

#[derive(Serialize)]
struct LoadAverageDto {
    one: f64,
    five: f64,
    fifteen: f64,
}

#[derive(Serialize)]
struct MemoryDto {
    total_bytes: u64,
    used_bytes: u64,
    free_bytes: u64,
    total_mib: u64,
    used_mib: u64,
    free_mib: u64,
}

#[derive(Serialize)]
struct ProcessDto {
    pid: String,
    name: String,
    cpu_usage_pct: f32,
    memory_bytes: u64,
    memory_mib: u64,
    virtual_memory_bytes: u64,
    virtual_memory_mib: u64,
    run_time_secs: u64,
}

#[derive(Serialize)]
struct HealthDto {
    generated_at_ist: String,
    hostname: Option<String>,
    os_name: Option<String>,
    os_version: Option<String>,
    kernel_version: Option<String>,
    uptime_secs: u64,
    cpu_cores: usize,
    cpu_usage_pct: f32,
    load_average: LoadAverageDto,
    memory: MemoryDto,
    swap: MemoryDto,
    current_process: Option<ProcessDto>,
}

fn mib(bytes: u64) -> u64 {
    bytes / 1024 / 1024
}

pub async fn health_handler() -> impl IntoResponse {
    let mut system = System::new_all();
    system.refresh_memory();
    system.refresh_cpu_usage();
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    system.refresh_cpu_usage();
    system.refresh_processes(ProcessesToUpdate::All, true);

    let load = System::load_average();
    let current_process = get_current_pid()
        .ok()
        .and_then(|pid| system.process(pid).map(|process| ProcessDto {
            pid: pid.to_string(),
            name: process.name().to_string_lossy().into_owned(),
            cpu_usage_pct: process.cpu_usage(),
            memory_bytes: process.memory(),
            memory_mib: mib(process.memory()),
            virtual_memory_bytes: process.virtual_memory(),
            virtual_memory_mib: mib(process.virtual_memory()),
            run_time_secs: process.run_time(),
        }));

    let total_memory = system.total_memory();
    let used_memory = system.used_memory();
    let free_memory = total_memory.saturating_sub(used_memory);
    let total_swap = system.total_swap();
    let used_swap = system.used_swap();
    let free_swap = total_swap.saturating_sub(used_swap);

    (
        StatusCode::OK,
        Json(HealthDto {
            generated_at_ist: shared_domain::current_ist_timestamp_string(),
            hostname: System::host_name(),
            os_name: System::name(),
            os_version: System::os_version(),
            kernel_version: System::kernel_version(),
            uptime_secs: System::uptime(),
            cpu_cores: system.cpus().len(),
            cpu_usage_pct: system.global_cpu_usage(),
            load_average: LoadAverageDto {
                one: load.one,
                five: load.five,
                fifteen: load.fifteen,
            },
            memory: MemoryDto {
                total_bytes: total_memory,
                used_bytes: used_memory,
                free_bytes: free_memory,
                total_mib: mib(total_memory),
                used_mib: mib(used_memory),
                free_mib: mib(free_memory),
            },
            swap: MemoryDto {
                total_bytes: total_swap,
                used_bytes: used_swap,
                free_bytes: free_swap,
                total_mib: mib(total_swap),
                used_mib: mib(used_swap),
                free_mib: mib(free_swap),
            },
            current_process,
        }),
    )
}