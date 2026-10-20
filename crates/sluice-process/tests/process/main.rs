mod adoption;
mod containment;
mod guardian;
mod launch;
mod limits;
mod locks;
mod tmux;

fn test_limits() -> sluice_model::RunLimits {
    sluice_model::RunLimits {
        memory_max_bytes: Some(1 << 30),
        cpu_millis: Some(2000),
        tasks: Some(512),
    }
}
