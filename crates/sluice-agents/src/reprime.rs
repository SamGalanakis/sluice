//! Compaction context is a fresh me snapshot, never an old transcript.
use std::path::Path;
pub fn context(task: &Path, snapshot: Result<&str, &str>) -> String {
    match snapshot {
        Ok(me) => format!(
            "Your context was just compacted. This is where your sluice step stands (`sluice me`); your full task is in {}.\n\n{me}",
            task.display()
        ),
        Err(error) => format!(
            "Your context was just compacted. Your full task is in {}; read it again before you continue (`sluice me` failed: {error}).",
            task.display()
        ),
    }
}
