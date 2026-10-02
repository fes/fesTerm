use std::{
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

pub(crate) fn simple_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub(crate) fn no_aliases(path: &Path) -> Result<(), String> {
    let mut ancestor = PathBuf::new();
    for component in path.components() {
        ancestor.push(component.as_os_str());
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        let metadata = match fs::symlink_metadata(&ancestor) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.to_string()),
        };
        let alias = metadata.file_type().is_symlink();
        #[cfg(windows)]
        let alias = {
            use std::os::windows::fs::MetadataExt;
            alias || metadata.file_attributes() & 0x400 != 0
        };
        if alias {
            return Err(format!(
                "fixture aliases/reparse points are forbidden: {}",
                ancestor.display()
            ));
        }
    }
    Ok(())
}

// Shared by the existing gallery ownership protocol and optional performance
// fixtures. Each protocol retains its own namespace and lifecycle records.
pub(crate) fn path_policy(root: &Path, run: &str, namespace: &str) -> Result<(), String> {
    if !simple_identity(run)
        || !root.is_absolute()
        || root.file_name().and_then(|name| name.to_str()) != Some(run)
        || root
            .components()
            .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
    {
        return Err("owned fixture root must be absolute, traversal-free and end in its explicit simple run identity".into());
    }
    #[cfg(windows)]
    if !matches!(root.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_)))
    {
        return Err(
            "owned fixture root must use a local ordinary drive path, not UNC/device syntax".into(),
        );
    }
    let control = root.parent().ok_or("missing fixture control directory")?;
    let target = control.parent().ok_or("missing fixture target directory")?;
    if control.file_name().and_then(|name| name.to_str()) != Some(namespace)
        || target.file_name().and_then(|name| name.to_str()) != Some("target")
    {
        return Err(format!(
            "owned fixture root must be <controlled-workspace>/target/{namespace}/<run>"
        ));
    }
    let normalized = root
        .to_str()
        .ok_or("fixture root must have a Unicode display identity")?
        .replace('\\', "/")
        .to_lowercase();
    for component in normalized.split('/') {
        if matches!(
            component,
            "." | ".."
                | "users"
                | "home"
                | "desktop"
                | "documents"
                | "downloads"
                | "appdata"
                | "onedrive"
                | "tmp"
                | "temp"
                | "windows"
                | "programdata"
                | "proc"
                | "sys"
                | "dev"
                | "etc"
        ) || component.starts_with("program files")
        {
            return Err("personal, system and temporary fixture roots are forbidden".into());
        }
    }
    for variable in [
        "HOME",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "TEMP",
        "TMP",
    ] {
        if let Some(value) = std::env::var_os(variable) {
            let excluded = value
                .to_string_lossy()
                .replace('\\', "/")
                .trim_end_matches('/')
                .to_lowercase();
            if !excluded.is_empty()
                && (normalized == excluded || normalized.starts_with(&format!("{excluded}/")))
            {
                return Err(format!("fixture root is inside excluded {variable} scope"));
            }
        }
    }
    for variable in ["USERNAME", "USER"] {
        if let Ok(username) = std::env::var(variable) {
            if !username.is_empty()
                && normalized
                    .split('/')
                    .any(|component| component == username.to_lowercase())
            {
                return Err(
                    "personal username components are forbidden in the controlled fixture scope"
                        .into(),
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn bytes(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    no_aliases(path)?;
    if !fs::symlink_metadata(path)
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err(format!(
            "not a regular owned fixture file: {}",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > limit {
        return Err(format!(
            "owned fixture file exceeds its bounded limit: {}",
            path.display()
        ));
    }
    Ok(bytes)
}

pub(crate) fn read_json(path: &Path) -> Result<serde_json::Value, String> {
    serde_json::from_slice(&bytes(path, 256 * 1024)?).map_err(|error| error.to_string())
}

pub(crate) fn write_new(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    no_aliases(path)?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("cannot claim {}: {error}", path.display()))?;
    file.write_all(&serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}
