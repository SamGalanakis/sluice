mod adoption;
mod containment;
mod guardian;
mod launch;
mod limits;
mod locks;
mod tmux;

fn test_limits() -> sluice_model::RunLimits {
    sluice_model::RunLimits {
        memory_bytes: 1 << 30,
        cpu_millis: 2000,
        tasks: 512,
    }
}
