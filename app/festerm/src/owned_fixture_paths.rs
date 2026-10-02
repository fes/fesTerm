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

#[derive(Default)]
struct PolicyInputs {
    scopes: Vec<(&'static str, String)>,
    usernames: Vec<String>,
}

impl PolicyInputs {
    fn capture() -> Self {
        Self {
            scopes: [
                "HOME",
                "USERPROFILE",
                "LOCALAPPDATA",
                "APPDATA",
                "TEMP",
                "TMP",
            ]
            .into_iter()
            .filter_map(|name| {
                std::env::var_os(name).map(|value| (name, value.to_string_lossy().into_owned()))
            })
            .collect(),
            usernames: ["USERNAME", "USER"]
                .into_iter()
                .filter_map(|name| std::env::var(name).ok())
                .collect(),
        }
    }
}

fn normalized(value: &str) -> String {
    let value = value.replace('\\', "/").to_lowercase();
    if value == "/" {
        value
    } else {
        value.trim_end_matches('/').to_owned()
    }
}

fn inside(path: &str, scope: &str) -> bool {
    path == scope
        || (scope == "/" && path.starts_with('/'))
        || path.starts_with(&format!("{scope}/"))
}

fn scope_policy(root: &str, workspace: &str, inputs: &PolicyInputs) -> Result<(), String> {
    let ancestors = workspace.split('/').count() - 1;
    for (index, component) in root.split('/').enumerate() {
        if matches!(
            component,
            "." | ".."
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
            || (matches!(component, "home" | "users") && index >= ancestors)
        {
            return Err("personal, system and temporary fixture roots are forbidden".into());
        }
    }
    for (variable, value) in &inputs.scopes {
        let excluded = normalized(value);
        if !excluded.is_empty() && inside(root, &excluded) {
            // Only the verified workspace's ancestry may be home-hosted. A
            // home directory itself, or anything inside the owned suffix, is
            // never exempted by repository markers.
            if matches!(*variable, "HOME" | "USERPROFILE")
                && workspace != excluded
                && inside(workspace, &excluded)
            {
                continue;
            }
            return Err(format!("fixture root is inside excluded {variable} scope"));
        }
    }
    for username in &inputs.usernames {
        if !username.is_empty()
            && root.split('/').enumerate().any(|(index, component)| {
                index >= ancestors && component == username.to_lowercase()
            })
        {
            return Err(
                "personal username components are forbidden in the controlled fixture scope".into(),
            );
        }
    }
    Ok(())
}

// Each protocol retains its own namespace and lifecycle records.
pub(crate) fn path_policy(root: &Path, run: &str, namespace: &str) -> Result<(), String> {
    path_policy_with_inputs(root, run, namespace, &PolicyInputs::capture())
}

fn path_policy_with_inputs(
    root: &Path,
    run: &str,
    namespace: &str,
    inputs: &PolicyInputs,
) -> Result<(), String> {
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
    let workspace = target.parent().ok_or("missing fixture workspace")?;
    no_aliases(root)?;
    for (name, directory_allowed) in [(".git", true), ("Cargo.toml", false)] {
        let marker = workspace.join(name);
        no_aliases(&marker)?;
        if !fs::metadata(&marker)
            .is_ok_and(|metadata| metadata.is_file() || (directory_allowed && metadata.is_dir()))
        {
            return Err("fixture root requires an existing controlled Git/Cargo workspace".into());
        }
    }
    scope_policy(
        &normalized(
            root.to_str()
                .ok_or("fixture root must have a Unicode display identity")?,
        ),
        &normalized(
            workspace
                .to_str()
                .ok_or("fixture workspace must have a Unicode display identity")?,
        ),
        inputs,
    )
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

#[cfg(test)]
pub(crate) struct TestWorkspace {
    directory: tempfile::TempDir,
    workspace: PathBuf,
}

#[cfg(test)]
impl TestWorkspace {
    pub(crate) fn new(label: &str) -> Self {
        assert!(simple_identity(label));
        let parent = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("target")
            .join("evidence");
        no_aliases(&parent).unwrap();
        fs::create_dir_all(&parent).unwrap();
        no_aliases(&parent).unwrap();
        let directory = tempfile::Builder::new()
            .prefix(&format!("fixture-workspace-{label}-"))
            .tempdir_in(&parent)
            .unwrap();
        let workspace = directory
            .path()
            .join("home")
            .join("fixture_account")
            .join("workspace");
        fs::create_dir_all(&workspace).unwrap();
        let git = std::process::Command::new("git")
            .args(["-c", "init.templateDir=", "init", "--quiet"])
            .arg(&workspace)
            .output()
            .unwrap();
        assert!(git.status.success(), "{git:?}");
        fs::write(workspace.join("Cargo.toml"), "[workspace]\n").unwrap();
        path_policy(
            &workspace
                .join("target")
                .join("ui-performance-owned-inputs")
                .join("ownership-check"),
            "ownership-check",
            "ui-performance-owned-inputs",
        )
        .unwrap();
        Self {
            directory,
            workspace,
        }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.workspace
    }

    pub(crate) fn home(&self) -> &Path {
        self.workspace.parent().unwrap()
    }

    pub(crate) fn case_path(&self) -> &Path {
        self.directory.path()
    }

    pub(crate) fn close(self) {
        let path = self.case_path().to_owned();
        no_aliases(&path).unwrap();
        self.directory.close().unwrap();
        assert!(!path.exists());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMESPACE: &str = "ui-performance-owned-inputs";

    fn inputs(home: &str, username: &str) -> PolicyInputs {
        PolicyInputs {
            scopes: vec![("HOME", home.into()), ("USERPROFILE", home.into())],
            usernames: vec![username.into(), username.into()],
        }
    }

    fn root(workspace: &Path, run: &str) -> PathBuf {
        workspace.join("target").join(NAMESPACE).join(run)
    }

    #[test]
    fn home_account_policy_covers_linux_macos_windows_without_environment_mutation() {
        for (home, workspace) in [
            ("/home/runner", "/home/runner/work/fesTerm/fesTerm"),
            ("/Users/runner", "/Users/runner/work/fesTerm/fesTerm"),
            ("C:\\Users\\runner", "C:\\Users\\runner\\work\\fesTerm"),
        ] {
            let home = normalized(home);
            let workspace = normalized(workspace);
            let inputs = inputs(&home, "runner");
            let root = format!("{workspace}/target/{NAMESPACE}/run");
            assert!(scope_policy(&root, &workspace, &inputs).is_ok());
            assert!(scope_policy(
                &format!("{home}/target/{NAMESPACE}/run"),
                &home,
                &PolicyInputs {
                    scopes: vec![("HOME", home.clone())],
                    usernames: Vec::new(),
                }
            )
            .is_err());
            for suffix in ["home", "Users", "runner", "tmp", "temp", "documents", "sys"] {
                assert!(scope_policy(
                    &normalized(&format!("{workspace}/target/{NAMESPACE}/{suffix}")),
                    &workspace,
                    &inputs
                )
                .is_err());
            }
            for variable in ["LOCALAPPDATA", "APPDATA", "TEMP", "TMP"] {
                assert!(scope_policy(
                    &root,
                    &workspace,
                    &PolicyInputs {
                        scopes: vec![(variable, home.clone())],
                        usernames: Vec::new(),
                    }
                )
                .is_err());
            }
        }
    }

    #[test]
    fn home_ancestry_requires_real_git_cargo_and_no_aliases() {
        let owned_workspace = TestWorkspace::new("verification");
        let home = owned_workspace.home();
        let workspace = owned_workspace.path();
        let inputs = inputs(home.to_str().unwrap(), "fixture_account");
        let root = root(workspace, "run");
        assert!(path_policy_with_inputs(&root, "run", NAMESPACE, &inputs).is_ok());
        for name in [".git", "Cargo.toml"] {
            let marker = workspace.join(name);
            let moved = workspace.join(format!("{name}-owned"));
            fs::rename(&marker, &moved).unwrap();
            assert!(path_policy_with_inputs(&root, "run", NAMESPACE, &inputs).is_err());
            fs::rename(moved, marker).unwrap();
        }
        let cargo = workspace.join("Cargo.toml");
        let cargo_owned = workspace.join("Cargo.toml-owned");
        fs::rename(&cargo, &cargo_owned).unwrap();
        fs::create_dir(&cargo).unwrap();
        assert!(path_policy_with_inputs(&root, "run", NAMESPACE, &inputs).is_err());
        fs::remove_dir(&cargo).unwrap();
        fs::rename(cargo_owned, cargo).unwrap();
        let marker = workspace.join(".git");
        let moved = workspace.join(".git-owned");
        fs::rename(&marker, &moved).unwrap();
        #[cfg(windows)]
        {
            let result = std::process::Command::new(std::env::var_os("COMSPEC").unwrap())
                .args(["/c", "mklink", "/J"])
                .arg(&marker)
                .arg(&moved)
                .output()
                .unwrap();
            assert!(result.status.success(), "{result:?}");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&moved, &marker).unwrap();
        let error = path_policy_with_inputs(&root, "run", NAMESPACE, &inputs).unwrap_err();
        assert!(error.contains("aliases/reparse points"), "{error}");
        #[cfg(windows)]
        fs::remove_dir(&marker).unwrap();
        #[cfg(unix)]
        fs::remove_file(&marker).unwrap();
        fs::rename(moved, marker).unwrap();
        owned_workspace.close();
    }

    #[test]
    fn verified_workspace_does_not_exempt_home_itself_or_unsafe_owned_suffixes() {
        let owned_workspace = TestWorkspace::new("exclusions");
        let home = owned_workspace.home();
        let workspace = owned_workspace.path();
        let fixture_root = root(workspace, "run");
        for variable in ["HOME", "USERPROFILE"] {
            for excluded in [workspace, fixture_root.as_path()] {
                let inputs = PolicyInputs {
                    scopes: vec![(variable, excluded.to_str().unwrap().into())],
                    usernames: Vec::new(),
                };
                let error =
                    path_policy_with_inputs(&fixture_root, "run", NAMESPACE, &inputs).unwrap_err();
                assert!(error.contains(variable), "{error}");
            }
        }
        let inputs = inputs(home.to_str().unwrap(), "fixture_account");
        for run in ["home", "Users", "fixture_account", "windows", "temp"] {
            assert!(
                path_policy_with_inputs(&root(workspace, run), run, NAMESPACE, &inputs).is_err()
            );
        }
        owned_workspace.close();
    }
}
