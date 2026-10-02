use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::owned_fixture_paths as owned;

const NAMESPACE: &str = "ui-performance-owned-inputs";
const LIMIT: u64 = 1024 * 1024;
pub(crate) const SCENES: [(&str, &str); 12] = [
    ("editor-syntax-2000-lines", "fixture.rs"),
    ("editor-find-2000-capped-matches", "fixture.rs"),
    ("sftp-100-rows-per-pane", "."),
    ("sftp-5000-rows-per-pane", "."),
    ("markdown-preview-400-sections", "fixture.md"),
    ("markdown-source-400-sections", "fixture.md"),
    ("markdown-source-find", "fixture.md"),
    ("markdown-preview-find", "fixture.md"),
    ("markdown-preview-headings", "markdown-preview-headings.md"),
    ("markdown-preview-prose", "markdown-preview-prose.md"),
    ("markdown-preview-code", "markdown-preview-code.md"),
    ("markdown-preview-tables", "markdown-preview-tables.md"),
];

pub(crate) fn rust_input() -> String {
    (0..2000)
        .map(|index| {
            format!("fn entry_{index}() {{ let value = {index}; println!(\"{{value}}\"); }}\n")
        })
        .collect()
}

pub(crate) fn markdown_input() -> String {
    (0..400)
        .map(|index| {
            format!(
                "## Section {index}\n\nA **synthetic** paragraph with `inline code` and ordinary text.\n\n\
                 ```rust\nfn example() {{ let value = {index}; }}\n```\n\n\
                 | Name | Value |\n| --- | --- |\n| Item | {index} |\n\n"
            )
        })
        .collect()
}

pub(crate) fn fragment_input(name: &str) -> String {
    (0..400)
        .map(|index| match name {
            "markdown-preview-headings" => format!("## Section {index}\n\n"),
            "markdown-preview-prose" => {
                "A **synthetic** paragraph with `inline code` and ordinary text.\n\n".into()
            }
            "markdown-preview-code" => {
                format!("```rust\nfn example() {{ let value = {index}; }}\n```\n\n")
            }
            "markdown-preview-tables" => {
                format!("| Name | Value |\n| --- | --- |\n| Item | {index} |\n\n")
            }
            _ => unreachable!(),
        })
        .collect()
}

fn corpus() -> Vec<(String, String)> {
    let mut files = vec![
        ("fixture.rs".into(), rust_input()),
        ("fixture.md".into(), markdown_input()),
        ("long-line.md".into(), "e\u{301} ".repeat(20_000)),
    ];
    files.extend(
        SCENES[8..]
            .iter()
            .map(|(name, file)| ((*file).into(), fragment_input(name))),
    );
    files
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn source_identity() -> serde_json::Value {
    serde_json::json!({
        "profile_sha256": digest(include_bytes!("surface_performance.rs")),
        "helper_sha256": digest(include_bytes!("performance_fixtures.rs")),
        "guards_sha256": digest(include_bytes!("owned_fixture_paths.rs")),
    })
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    path: String,
    kind: String,
    bytes: u64,
    sha256: Option<String>,
    modified_unix_nanos: u128,
}

fn inventory(inputs: &Path) -> Result<Vec<Entry>, String> {
    fn entry(path: &Path, relative: String) -> Result<Entry, String> {
        owned::no_aliases(path)?;
        let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
        let (kind, sha256, bytes) = if metadata.is_dir() {
            ("directory", None, 0)
        } else if metadata.is_file() {
            let bytes = owned::bytes(path, LIMIT)?;
            ("file", Some(digest(&bytes)), bytes.len() as u64)
        } else {
            return Err("unsupported physical fixture entry".into());
        };
        Ok(Entry {
            path: relative,
            kind: kind.into(),
            bytes,
            sha256,
            modified_unix_nanos: metadata
                .modified()
                .map_err(|error| error.to_string())?
                .duration_since(UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos(),
        })
    }
    let mut result = vec![entry(inputs, ".".into())?];
    for child in fs::read_dir(inputs).map_err(|error| error.to_string())? {
        let child = child.map_err(|error| error.to_string())?;
        if result.len() >= 32 {
            return Err("physical fixture inventory exceeds its entry bound".into());
        }
        let name = child
            .file_name()
            .into_string()
            .map_err(|_| "non-Unicode physical fixture identity")?;
        let child = entry(&child.path(), name)?;
        if child.kind != "file" {
            return Err("physical performance inputs must be flat regular files".into());
        }
        result.push(child);
    }
    result.sort_by(|a, b| a.path.cmp(&b.path));
    if result.iter().map(|entry| entry.bytes).sum::<u64>() > LIMIT {
        return Err("physical fixture corpus exceeds one MiB".into());
    }
    Ok(result)
}

fn check_corpus(entries: &[Entry]) -> Result<(), String> {
    let expected = corpus();
    if entries.len() != expected.len() + 1 {
        return Err("physical fixture inventory has missing or unowned entries".into());
    }
    for (name, contents) in expected {
        let entry = entries.iter().find(|entry| entry.path == name);
        if !entry.is_some_and(|entry| {
            entry.kind == "file"
                && entry.bytes == contents.len() as u64
                && entry.sha256.as_deref() == Some(digest(contents.as_bytes()).as_str())
        }) {
            return Err(format!("physical fixture corpus differs at {name}"));
        }
    }
    Ok(())
}

fn configuration(
    root: Option<std::ffi::OsString>,
    run: Option<String>,
    step: Option<String>,
) -> Result<Option<(PathBuf, String, Option<String>)>, String> {
    match (root, run, step) {
        (None, None, None) => Ok(None),
        (Some(root), Some(run), step) if step.as_deref().is_none_or(owned::simple_identity) => {
            Ok(Some((root.into(), run, step)))
        }
        _ => Err(
            "physical fixtures require ROOT/RUN and a simple unique STEP for each profile process"
                .into(),
        ),
    }
}

fn environment() -> Result<Option<(PathBuf, String, Option<String>)>, String> {
    fn value(name: &str) -> Result<Option<String>, String> {
        match std::env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }
    configuration(
        std::env::var_os("FESTERM_SURFACE_FIXTURE_ROOT"),
        value("FESTERM_SURFACE_FIXTURE_RUN")?,
        value("FESTERM_SURFACE_FIXTURE_STEP")?,
    )
}

fn validate(root: &Path, run: &str) -> Result<serde_json::Value, String> {
    owned::path_policy(root, run, NAMESPACE)?;
    owned::no_aliases(root)?;
    let control = root.parent().unwrap();
    let workspace = control.parent().unwrap().parent().unwrap();
    for marker in [workspace.join(".git"), workspace.join("Cargo.toml")] {
        owned::no_aliases(&marker)?;
        if !marker.exists() {
            return Err("physical fixture root requires an existing Git/Cargo workspace".into());
        }
    }
    let control_owner = serde_json::json!({
        "schema": "festerm-performance-control-v1",
        "workspace": fs::canonicalize(workspace).map_err(|error| error.to_string())?,
    });
    if owned::read_json(&control.join(".owner.json"))? != control_owner {
        return Err("unowned physical performance namespace".into());
    }
    let ready = owned::read_json(&root.join("ready.json"))?;
    if ready["schema"] != "festerm-performance-fixtures-v1"
        || ready["physical_root"] != serde_json::json!(root)
        || ready["run"] != run
        || ready["source"] != source_identity()
        || owned::read_json(&control.join(format!(".claimed-run-{run}.json")))? != ready["owner"]
    {
        return Err("physical fixture owner/run/probe source differs".into());
    }
    if root.join("cleanup.complete.json").exists() {
        return Err("completed physical fixture identity cannot be reused".into());
    }
    let actual = inventory(&root.join("inputs"))?;
    check_corpus(&actual)?;
    if serde_json::to_value(actual).map_err(|error| error.to_string())? != ready["inventory"] {
        return Err("physical input SHA/mtime/kind/size differs; retaining inputs".into());
    }
    let children: BTreeSet<_> = fs::read_dir(root)
        .map_err(|error| error.to_string())?
        .map(|entry| {
            entry.map_err(|error| error.to_string()).and_then(|entry| {
                entry
                    .file_name()
                    .into_string()
                    .map_err(|_| "non-Unicode scope entry".into())
            })
        })
        .collect::<Result<_, String>>()?;
    let mut expected: BTreeSet<_> = ["inputs", "ready.json", "uses"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    if root.join("active.json").exists() {
        expected.insert("active.json".into());
    }
    if children != expected {
        return Err("physical fixture scope has unowned entries; refusing it".into());
    }
    Ok(ready)
}

fn prepare(root: &Path, run: &str, plan: &[String]) -> Result<(), String> {
    owned::path_policy(root, run, NAMESPACE)?;
    owned::no_aliases(root)?;
    if plan.is_empty()
        || plan.len() > 32
        || plan.iter().any(|id| !owned::simple_identity(id))
        || plan.iter().collect::<BTreeSet<_>>().len() != plan.len()
    {
        return Err("prepare requires a bounded nonempty unique process STEP plan".into());
    }
    let control = root.parent().unwrap();
    let target = control.parent().unwrap();
    let workspace = target.parent().unwrap();
    for marker in [workspace.join(".git"), workspace.join("Cargo.toml")] {
        owned::no_aliases(&marker)?;
        if !marker.exists() {
            return Err("prepare requires an existing controlled Git/Cargo workspace".into());
        }
    }
    let control_owner = serde_json::json!({
        "schema": "festerm-performance-control-v1",
        "workspace": fs::canonicalize(workspace).map_err(|error| error.to_string())?,
    });
    if !target.exists() {
        fs::create_dir(target).map_err(|error| error.to_string())?;
    }
    if control.exists() {
        if owned::read_json(&control.join(".owner.json"))? != control_owner {
            return Err("existing physical fixture namespace is unowned".into());
        }
    } else {
        fs::create_dir(control).map_err(|error| error.to_string())?;
        owned::write_new(&control.join(".owner.json"), &control_owner)?;
    }
    if root.exists() {
        return Err("prepare refuses an existing run, even empty; choose a fresh identity".into());
    }
    let owner = serde_json::json!({
        "schema": "festerm-performance-owner-v1", "physical_root": root, "run": run, "plan": plan,
    });
    owned::write_new(&control.join(format!(".claimed-run-{run}.json")), &owner)?;
    fs::create_dir(root).map_err(|error| error.to_string())?;
    let inputs = root.join("inputs");
    fs::create_dir(&inputs).map_err(|error| error.to_string())?;
    fs::create_dir(root.join("uses")).map_err(|error| error.to_string())?;
    for (name, contents) in corpus() {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(inputs.join(name))
            .map_err(|error| error.to_string())?;
        file.write_all(contents.as_bytes())
            .map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
    }
    let inventory = inventory(&inputs)?;
    check_corpus(&inventory)?;
    owned::write_new(
        &root.join("ready.json"),
        &serde_json::json!({
            "schema": "festerm-performance-fixtures-v1", "physical_root": root, "run": run,
            "owner": owner, "source": source_identity(), "inventory": inventory,
            "scenes": SCENES, "microprobe_scope": "unchanged in-memory fence loads/syntax setup; long-line.md is retained physical input",
        }),
    )?;
    validate(root, run)?;
    Ok(())
}

pub(crate) struct PerformanceFixtures {
    temporary: Option<tempfile::TempDir>,
    input_path: PathBuf,
    shared: Option<(PathBuf, String, String, serde_json::Value)>,
    observations: Vec<serde_json::Value>,
}

impl PerformanceFixtures {
    pub(crate) fn from_environment() -> Result<Self, String> {
        match environment()? {
            None => Self::temporary(),
            Some((root, run, Some(step))) => Self::begin(root, run, step),
            _ => Err(
                "shared profile requires its planned unique FESTERM_SURFACE_FIXTURE_STEP".into(),
            ),
        }
    }

    fn temporary() -> Result<Self, String> {
        let temporary = tempfile::tempdir().map_err(|error| error.to_string())?;
        Ok(Self {
            input_path: temporary.path().to_owned(),
            temporary: Some(temporary),
            shared: None,
            observations: Vec::new(),
        })
    }

    fn begin(root: PathBuf, run: String, step: String) -> Result<Self, String> {
        let ready = validate(&root, &run)?;
        if !owned::simple_identity(&step)
            || !ready["owner"]["plan"]
                .as_array()
                .is_some_and(|plan| plan.iter().any(|id| id == &step))
        {
            return Err("profile STEP is not in the prepared unique process plan".into());
        }
        if root
            .join("uses")
            .join(format!("{step}.started.json"))
            .exists()
        {
            return Err(
                "profile STEP already claimed; retain the attempt, never retry/relabel it".into(),
            );
        }
        let start = serde_json::json!({
            "step": step, "run": run, "pid": std::process::id(),
            "ready_sha256": digest(&owned::bytes(&root.join("ready.json"), 256 * 1024)?),
        });
        owned::write_new(&root.join("active.json"), &start)?;
        owned::write_new(
            &root.join("uses").join(format!("{step}.started.json")),
            &start,
        )?;
        Ok(Self {
            temporary: None,
            input_path: root.join("inputs"),
            shared: Some((root, run, step, ready)),
            observations: Vec::new(),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        self.temporary
            .as_ref()
            .map_or(self.input_path.as_path(), |temporary| temporary.path())
    }

    pub(crate) fn write_input(&self, path: &Path, contents: &str) -> Result<(), String> {
        if let Some((root, run, _, _)) = &self.shared {
            validate(root, run)?;
            if path.parent() != Some(self.path())
                || owned::bytes(path, LIMIT)? != contents.as_bytes()
            {
                return Err(
                    "profile attempted a different physical input; no rewrite performed".into(),
                );
            }
            Ok(())
        } else {
            fs::write(path, contents).map_err(|error| error.to_string())
        }
    }

    pub(crate) fn verify_fragment(&self, path: &Path, contents: &str) -> Result<(), String> {
        if self.shared.is_some() {
            self.write_input(path, contents)
        } else {
            Ok(())
        }
    }

    pub(crate) fn observe(&mut self, name: &str, actual_path: &Path) -> Result<(), String> {
        let Some((root, run, _, _)) = &self.shared else {
            return Ok(());
        };
        let ready = validate(root, run)?;
        let (_, file) = SCENES
            .get(self.observations.len())
            .ok_or("too many scene identities")?;
        if SCENES[self.observations.len()].0 != name || actual_path != self.path().join(file) {
            return Err("observed actual scene identity/order differs from all12 schema".into());
        }
        let entry = ready["inventory"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["path"] == *file)
            .ok_or("scene input missing from inventory")?;
        self.observations.push(serde_json::json!({
            "scene": name, "actual_input_path": actual_path,
            "canonical_input_path": fs::canonicalize(actual_path).map_err(|error| error.to_string())?,
            "input": entry,
            "basis": "observed physical path; profile callers bind real constructors, protocol-only checks do not execute widgets; no display-label substitution",
        }));
        Ok(())
    }

    pub(crate) fn finish(
        &self,
        output: &Path,
        report: Option<&serde_json::Value>,
    ) -> Result<(), String> {
        let Some((root, run, step, expected)) = &self.shared else {
            return Ok(());
        };
        let actual = validate(root, run)?;
        if &actual != expected || self.observations.len() != SCENES.len() {
            return Err("incomplete or changed all12 physical input observations".into());
        }
        owned::no_aliases(output)?;
        let canonical_output = fs::canonicalize(output).map_err(|error| error.to_string())?;
        let canonical_root = fs::canonicalize(root).map_err(|error| error.to_string())?;
        if canonical_output.starts_with(&canonical_root)
            || canonical_root.starts_with(&canonical_output)
        {
            return Err(
                "performance output and owned physical fixture root must be disjoint".into(),
            );
        }
        let samples = if let Some(report) = report {
            let samples = report["samples"]
                .as_array()
                .ok_or("profile samples missing")?;
            if samples.len() < 12
                || SCENES
                    .iter()
                    .zip(samples)
                    .any(|((name, _), sample)| sample["name"] != *name)
            {
                return Err("completed report dropped/reordered an original control".into());
            }
            serde_json::json!(samples.iter().take(12).map(|sample| serde_json::json!({
                "name": sample["name"], "items": sample["items"], "viewport_points": sample["viewport_points"],
                "last_shape_count": sample["last_shape_count"], "last_vertex_count": sample["last_vertex_count"],
            })).collect::<Vec<_>>())
        } else {
            serde_json::Value::Null
        };
        let proof = serde_json::json!({
            "schema": "festerm-performance-fixture-proof-v1", "fixture_identity": actual,
            "observed_identities": self.observations, "all12_sample_structure": samples,
            "step": step, "execution": if report.is_some() { "profile" } else { "protocol-only; no UI/timing evidence" },
            "boundary": "physical input identity only, not pixel equality, latency, native acceptance, anonymity or performance gain",
        });
        owned::write_new(&output.join("fixture-proof.json"), &proof)?;
        owned::write_new(
            &root.join("uses").join(format!("{step}.complete.json")),
            &serde_json::json!({
                "step": step, "run": run, "output": canonical_output,
                "proof_sha256": digest(&owned::bytes(&output.join("fixture-proof.json"), 256 * 1024)?),
            }),
        )?;
        if owned::read_json(&root.join("active.json"))?["step"] != *step {
            return Err("active physical fixture ownership changed; retaining claim".into());
        }
        fs::remove_file(root.join("active.json")).map_err(|error| error.to_string())
    }
}

fn cleanup(root: &Path, run: &str) -> Result<(), String> {
    let ready = validate(root, run)?;
    if root.join("active.json").exists() {
        return Err("active/failed process retained; refuse cleanup".into());
    }
    let plan = ready["owner"]["plan"]
        .as_array()
        .ok_or("missing process plan")?;
    let expected: BTreeSet<_> = plan
        .iter()
        .flat_map(|id| {
            let id = id.as_str().unwrap();
            [format!("{id}.started.json"), format!("{id}.complete.json")]
        })
        .collect();
    let actual: BTreeSet<_> = fs::read_dir(root.join("uses"))
        .map_err(|error| error.to_string())?
        .map(|entry| {
            entry
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .map_err(|error| error.to_string())
        })
        .collect::<Result<_, _>>()?;
    if actual != expected {
        return Err("process plan incomplete or unowned use record; refuse cleanup".into());
    }
    for id in plan {
        let id = id.as_str().unwrap();
        let complete = owned::read_json(&root.join("uses").join(format!("{id}.complete.json")))?;
        let output = PathBuf::from(
            complete["output"]
                .as_str()
                .ok_or("invalid completed output")?,
        );
        if complete["step"] != id
            || complete["run"] != run
            || complete["proof_sha256"]
                != digest(&owned::bytes(
                    &output.join("fixture-proof.json"),
                    256 * 1024,
                )?)
        {
            return Err("completed process proof changed; refuse cleanup".into());
        }
    }
    for entry in ready["inventory"].as_array().unwrap() {
        if entry["kind"] == "file" {
            let path = root.join("inputs").join(entry["path"].as_str().unwrap());
            owned::no_aliases(&path)?;
            fs::remove_file(path).map_err(|error| error.to_string())?;
        }
    }
    fs::remove_dir(root.join("inputs")).map_err(|error| error.to_string())?;
    owned::write_new(
        &root.join("cleanup.complete.json"),
        &serde_json::json!({
            "run": run, "ready_sha256": digest(&owned::bytes(&root.join("ready.json"), 256 * 1024)?),
            "plan": plan, "tracked_input_cleanup": true,
        }),
    )
}

#[test]
#[ignore = "optional physical fixture prepare/verify/cleanup only; no UI, GPU or timing"]
fn shared_performance_fixture_protocol() {
    assert_eq!(
        std::env::var("FESTERM_RUN_OPTIONAL_VALIDATION").as_deref(),
        Ok("1")
    );
    let (root, run, step) = environment().unwrap().expect("set performance ROOT/RUN");
    match std::env::var("FESTERM_SURFACE_FIXTURE_ACTION")
        .unwrap()
        .as_str()
    {
        "prepare" => {
            assert!(step.is_none());
            let plan: Vec<_> = std::env::var("FESTERM_SURFACE_FIXTURE_PLAN")
                .unwrap()
                .split(',')
                .map(str::to_owned)
                .collect();
            prepare(&root, &run, &plan).unwrap();
        }
        "verify" => {
            assert!(step.is_none());
            validate(&root, &run).unwrap();
        }
        "cleanup" => {
            assert!(step.is_none());
            cleanup(&root, &run).unwrap();
        }
        "check-step" => {
            let mut fixtures = PerformanceFixtures::begin(
                root.clone(),
                run.clone(),
                step.expect("set unique STEP"),
            )
            .unwrap();
            for (name, file) in SCENES {
                fixtures.observe(name, &fixtures.path().join(file)).unwrap();
            }
            let output = PathBuf::from(
                std::env::var_os("FESTERM_SURFACE_PROFILE_OUT").expect("fresh explicit OUT"),
            );
            assert!(!output.exists());
            fs::create_dir_all(&output).unwrap();
            fixtures.finish(&output, None).unwrap();
        }
        _ => panic!("fixture action must be prepare, verify, check-step or cleanup"),
    }
    println!(
        "performance physical fixture protocol complete: {} ({run}); no timing/UI evidence",
        root.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn shared_corpus_matches_unchanged_clean_nav_reference_bytes() {
        let expected = [
            (
                "fixture.rs",
                "97b7c30d44900dbd8da5bd424d917449f8f22d8432af3e2ad25d6a2d4676dc2a",
            ),
            (
                "fixture.md",
                "e4133f450da40c43e05a816b4b585a613e1b31cfbf2897123975ec74aec47fd7",
            ),
            (
                "long-line.md",
                "ce7b8ecdb44df1c02cc59b7a3f7ad5e14edd4a449b1b54e65447d27b2de16aba",
            ),
            (
                "markdown-preview-headings.md",
                "77cbdeff7b2f3292ef81cc4f16789b1d2f97a1d4b40c26ede6e818bdfcac50e0",
            ),
            (
                "markdown-preview-prose.md",
                "a18d897b991cb30c957471ad7201dbd987a6ff474a4d490270d5f0623d15fd95",
            ),
            (
                "markdown-preview-code.md",
                "dbf6ff440e48b311e55c499bbd8f5d7435438f0013e577e6dc4fc2977b8c0d8b",
            ),
            (
                "markdown-preview-tables.md",
                "d51c6bc177c933dc1305e5521b3006cd412216918c99429fc71d1c3fb02ef48a",
            ),
        ];
        for ((name, contents), (expected_name, sha256)) in corpus().into_iter().zip(expected) {
            assert_eq!(name, expected_name);
            assert_eq!(digest(contents.as_bytes()), sha256);
        }
    }

    fn scope(label: &str) -> (PathBuf, String) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let run = format!(
            "guard-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        (workspace.join("target").join(NAMESPACE).join(&run), run)
    }

    fn complete(root: &Path, run: &str, step: &str) -> serde_json::Value {
        let mut fixtures =
            PerformanceFixtures::begin(root.into(), run.into(), step.into()).unwrap();
        for (name, file) in SCENES {
            fixtures.observe(name, &fixtures.path().join(file)).unwrap();
        }
        let output = root
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("evidence")
            .join(format!("{run}-{step}"));
        fs::create_dir_all(&output).unwrap();
        fixtures.finish(&output, None).unwrap();
        owned::read_json(&output.join("fixture-proof.json")).unwrap()
    }

    #[test]
    fn shared_configuration_and_unset_mode_preserve_ephemeral_behavior() {
        assert!(configuration(None, None, None).unwrap().is_none());
        assert!(configuration(None, Some("run".into()), None).is_err());
        assert!(configuration(Some("Q:\\root".into()), None, None).is_err());
        assert!(configuration(
            Some("Q:\\root".into()),
            Some("root".into()),
            Some("../step".into())
        )
        .is_err());
        let path = {
            let fixture = PerformanceFixtures::temporary().unwrap();
            let path = fixture.path().to_owned();
            fixture
                .write_input(&path.join("fixture.rs"), "first")
                .unwrap();
            fixture
                .write_input(&path.join("fixture.rs"), "second")
                .unwrap();
            assert_eq!(
                fs::read_to_string(path.join("fixture.rs")).unwrap(),
                "second"
            );
            fixture
                .verify_fragment(&path.join("absent.md"), "in memory")
                .unwrap();
            assert!(!path.join("absent.md").exists());
            path
        };
        assert!(!path.exists());
    }

    #[test]
    fn shared_actual_paths_all12_and_metadata_match_across_uses_without_rewrites() {
        let (root, run) = scope("parity");
        prepare(&root, &run, &["A1".into(), "B1".into()]).unwrap();
        let before = inventory(&root.join("inputs")).unwrap();
        let a = complete(&root, &run, "A1");
        let b = complete(&root, &run, "B1");
        assert_eq!(a["fixture_identity"], b["fixture_identity"]);
        assert_eq!(a["observed_identities"], b["observed_identities"]);
        assert_eq!(a["observed_identities"].as_array().unwrap().len(), 12);
        assert_eq!(before, inventory(&root.join("inputs")).unwrap());
        cleanup(&root, &run).unwrap();
        assert!(!root.join("inputs").exists());
        assert!(root.join("cleanup.complete.json").exists());
        assert!(prepare(&root, &run, &["A1".into()]).is_err());
        assert!(PerformanceFixtures::begin(root.clone(), run.clone(), "A1".into()).is_err());
    }

    #[test]
    fn shared_plan_reuse_active_ownership_and_incomplete_schema_are_rejected() {
        let (root, run) = scope("claims");
        prepare(&root, &run, &["A1".into(), "B1".into()]).unwrap();
        assert!(prepare(&root, &run, &["A1".into()]).is_err());
        assert!(PerformanceFixtures::begin(root.clone(), run.clone(), "other".into()).is_err());
        let mut a = PerformanceFixtures::begin(root.clone(), run.clone(), "A1".into()).unwrap();
        assert!(PerformanceFixtures::begin(root.clone(), run.clone(), "B1".into()).is_err());
        assert!(cleanup(&root, &run).is_err());
        let wrong = a.path().join("fixture.md");
        assert!(a.observe("editor-syntax-2000-lines", &wrong).is_err());
        for (name, file) in SCENES {
            a.observe(name, &a.path().join(file)).unwrap();
        }
        let output = root
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("evidence")
            .join(format!("{run}-A1"));
        fs::create_dir_all(&output).unwrap();
        let bad_report = serde_json::json!({"samples":[{"name":"wrong"}]});
        assert!(a.finish(&output, Some(&bad_report)).is_err());
        a.finish(&output, None).unwrap();
        assert!(PerformanceFixtures::begin(root.clone(), run.clone(), "A1".into()).is_err());
        assert!(cleanup(&root, &run).is_err());
        complete(&root, &run, "B1");
        cleanup(&root, &run).unwrap();
    }

    #[test]
    fn shared_content_mtime_and_unknown_entries_are_errors_not_freshened() {
        let (root, run) = scope("content");
        prepare(&root, &run, &["A1".into()]).unwrap();
        let path = root.join("inputs").join("fixture.rs");
        let original = fs::read(&path).unwrap();
        fs::write(&path, b"changed").unwrap();
        assert!(validate(&root, &run).is_err());
        assert!(cleanup(&root, &run).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"changed");
        assert_ne!(original, fs::read(&path).unwrap());

        let (root, run) = scope("mtime");
        prepare(&root, &run, &["A1".into()]).unwrap();
        let path = root.join("inputs").join("fixture.md");
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(
                fs::FileTimes::new().set_modified(modified + std::time::Duration::from_secs(1)),
            )
            .unwrap();
        assert!(validate(&root, &run).is_err());
        assert!(cleanup(&root, &run).is_err());

        let (root, run) = scope("unknown");
        prepare(&root, &run, &["A1".into()]).unwrap();
        fs::write(root.join("inputs").join("unowned.txt"), b"retain").unwrap();
        assert!(validate(&root, &run).is_err());
        assert!(cleanup(&root, &run).is_err());
        assert!(root.join("inputs").join("unowned.txt").exists());
    }

    #[test]
    fn shared_root_policy_rejects_unsafe_unowned_and_alias_scopes() {
        let (root, run) = scope("policy");
        for unsafe_root in [
            PathBuf::from("relative").join(&run),
            root.parent().unwrap().join("..").join(&run),
            root.parent().unwrap().join("different"),
            PathBuf::from("Q:\\Users\\person\\target")
                .join(NAMESPACE)
                .join(&run),
            PathBuf::from("\\\\server\\share\\target")
                .join(NAMESPACE)
                .join(&run),
            PathBuf::from("\\\\?\\Q:\\target")
                .join(NAMESPACE)
                .join(&run),
        ] {
            assert!(
                owned::path_policy(&unsafe_root, &run, NAMESPACE).is_err(),
                "{unsafe_root:?}"
            );
        }
        fs::create_dir_all(&root).unwrap();
        assert!(prepare(&root, &run, &["A1".into()]).is_err());
        assert!(validate(&root, &run).is_err());
        fs::remove_dir(&root).unwrap();
        #[cfg(windows)]
        {
            let (alias, alias_run) = scope("junction");
            let target = alias
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("evidence")
                .join(&alias_run);
            fs::create_dir_all(&target).unwrap();
            let status = std::process::Command::new(std::env::var_os("COMSPEC").unwrap())
                .args(["/c", "mklink", "/J"])
                .arg(&alias)
                .arg(&target)
                .output()
                .unwrap();
            assert!(status.status.success(), "{:?}", status.stderr);
            assert!(owned::no_aliases(&alias).is_err());
            assert!(prepare(&alias, &alias_run, &["A1".into()]).is_err());
            fs::remove_dir(&alias).unwrap();
            fs::remove_dir(&target).unwrap();
        }
    }
}
