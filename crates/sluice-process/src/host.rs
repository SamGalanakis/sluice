use sluice_model::error::PublicError;
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

pub const OWNER_HOME: &str = "/home/sam/.sluice";

/// Resolve existing symlinks and normalize a possibly nonexistent suffix, in path order.
pub fn resolve_path(path: &Path) -> Result<PathBuf, PublicError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(storage)?.join(path)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::RootDir => resolved.push("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(part) => {
                resolved.push(part);
                match fs::symlink_metadata(&resolved) {
                    Ok(_) => {
                        resolved = fs::canonicalize(&resolved).map_err(storage)?;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(storage(e)),
                }
            }
            Component::Prefix(_) => {
                return Err(PublicError::BadRequest {
                    message: "unsupported path prefix".into(),
                });
            }
        }
    }
    Ok(resolved)
}
fn storage(error: std::io::Error) -> PublicError {
    PublicError::Storage {
        message: error.to_string(),
    }
}

/// Reject the protected home and every descendant before creating or opening writable state.
pub fn guard_home(path: &Path, owner_home: &Path) -> Result<PathBuf, PublicError> {
    let resolved = resolve_path(path)?;
    let protected = resolve_path(owner_home)?;
    if resolved.starts_with(protected) {
        return Err(PublicError::BadRequest {
            message: "refusing the owner's real SLUICE_HOME or a path under it".into(),
        });
    }
    Ok(resolved)
}

pub fn guard_scratch_home(path: &Path) -> Result<PathBuf, PublicError> {
    let resolved = guard_home(path, Path::new(OWNER_HOME))?;
    if let Some(home) = std::env::var_os("HOME") {
        guard_home(&resolved, &PathBuf::from(home).join(".sluice"))?;
    }
    Ok(resolved)
}
