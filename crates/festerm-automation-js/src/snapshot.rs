use crate::Error;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_SOURCE_BYTES: usize = 1024 * 1024;
pub const MAX_SNAPSHOT_BYTES: usize = 2 * MAX_SOURCE_BYTES;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub version: u16,
    pub api_version: u16,
    pub name: String,
    pub entry: String,
    /// Exact, canonical paths; these must equal the supplied source set.
    pub modules: Vec<String>,
    #[serde(deserialize_with = "unique_map")]
    pub parameters: BTreeMap<String, ParameterType>,
    /// Maximum requested effects only, never a grant.
    pub capabilities: BTreeSet<Capability>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "camelCase")]
pub enum Capability {
    Observe,
    ReadScreen,
    ReadScrollback,
    GetSelection,
    WaitForChange,
    SendText,
    SendKeys,
    Open,
    InvokeWorkflow,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ParameterType {
    Text { max_bytes: u16 },
    Integer { min: i64, max: i64 },
    Boolean {},
    Target {},
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    content = "value",
    rename_all = "camelCase",
    deny_unknown_fields
)]
pub enum ParameterValue {
    Text(String),
    Integer(i64),
    Boolean(bool),
    Target(String),
}

pub type Parameters = BTreeMap<String, ParameterValue>;

/// Run-local parameter values and the exact generations for target values.
/// These are references, not grants; the application must derive them from its
/// frozen invocation and still authorize every host call.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BoundParameters {
    bundle_digest: String,
    #[serde(deserialize_with = "unique_map")]
    values: Parameters,
    #[serde(deserialize_with = "unique_map")]
    target_generations: BTreeMap<String, u64>,
}

impl std::fmt::Debug for BoundParameters {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("BoundParameters(<private>)")
    }
}

impl BoundParameters {
    pub fn values(&self) -> &Parameters {
        &self.values
    }

    pub fn target(&self, id: &str) -> Option<crate::protocol::Target> {
        self.target_generations
            .get(id)
            .map(|generation| crate::protocol::Target {
                id: id.to_owned(),
                generation: *generation,
            })
    }

    pub(crate) fn validate(&self, bundle: &ApprovedBundle) -> Result<(), Error> {
        if self.bundle_digest != bundle.digest() {
            return Err(Error::Snapshot);
        }
        bundle.validate_parameters(&self.values)?;
        let referenced: BTreeSet<_> = self
            .values
            .values()
            .filter_map(|value| match value {
                ParameterValue::Target(id) => Some(id.as_str()),
                _ => None,
            })
            .collect();
        if referenced.len() != self.target_generations.len()
            || self.target_generations.iter().any(|(id, generation)| {
                !referenced.contains(id.as_str()) || *generation > crate::protocol::MAX_SAFE_INTEGER
            })
        {
            return Err(Error::Snapshot);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSnapshot {
    pub manifest: Manifest,
    #[serde(deserialize_with = "unique_map")]
    pub modules: BTreeMap<String, String>,
}

pub(crate) fn unique_map<'de, D, V>(deserializer: D) -> Result<BTreeMap<String, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    V: Deserialize<'de>,
{
    struct Unique<V>(std::marker::PhantomData<V>);
    impl<'de, V: Deserialize<'de>> serde::de::Visitor<'de> for Unique<V> {
        type Value = BTreeMap<String, V>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a map with unique keys")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut access: A,
        ) -> Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            while let Some((key, value)) = access.next_entry()? {
                if values.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate key"));
                }
            }
            Ok(values)
        }
    }
    deserializer.deserialize_map(Unique(std::marker::PhantomData))
}

/// A frozen in-memory snapshot; deliberately has no path/file/bytecode loader.
#[derive(Clone)]
pub struct ApprovedBundle {
    snapshot: SourceSnapshot,
    digest: String,
}

impl std::fmt::Debug for ApprovedBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApprovedBundle")
            .field("digest", &self.digest)
            .finish_non_exhaustive()
    }
}

impl ApprovedBundle {
    pub fn new(snapshot: SourceSnapshot) -> Result<Self, Error> {
        validate(&snapshot)?;
        let canonical = serde_json::to_vec(&snapshot).map_err(|_| Error::Snapshot)?;
        if canonical.len() > MAX_SNAPSHOT_BYTES {
            return Err(Error::Snapshot);
        }
        let digest = format!("{:x}", Sha256::digest(&canonical));
        Ok(Self { snapshot, digest })
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }

    pub fn manifest(&self) -> &Manifest {
        &self.snapshot.manifest
    }

    pub fn snapshot(&self) -> &SourceSnapshot {
        &self.snapshot
    }

    pub fn bind_parameters(
        &self,
        values: Parameters,
        target_generations: BTreeMap<String, u64>,
    ) -> Result<BoundParameters, Error> {
        let bound = BoundParameters {
            bundle_digest: self.digest.clone(),
            values,
            target_generations,
        };
        bound.validate(self)?;
        Ok(bound)
    }

    pub fn validate_parameters(&self, values: &Parameters) -> Result<(), Error> {
        if values.len() != self.manifest().parameters.len() {
            return Err(Error::Snapshot);
        }
        for (key, ty) in &self.manifest().parameters {
            let valid = match (ty, values.get(key)) {
                (ParameterType::Text { max_bytes }, Some(ParameterValue::Text(v))) => {
                    v.len() <= usize::from(*max_bytes)
                }
                (ParameterType::Integer { min, max }, Some(ParameterValue::Integer(v))) => {
                    min <= v && v <= max
                }
                (ParameterType::Boolean {}, Some(ParameterValue::Boolean(_))) => true,
                (ParameterType::Target {}, Some(ParameterValue::Target(v))) => token(v),
                _ => false,
            };
            if !valid {
                return Err(Error::Snapshot);
            }
        }
        if serde_json::to_vec(values)
            .map_err(|_| Error::Snapshot)?
            .len()
            > 32_768
        {
            return Err(Error::Snapshot);
        }
        Ok(())
    }
}

pub(crate) fn token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

fn path(value: &str) -> bool {
    value.len() <= 256
        && value.ends_with(".js")
        && value.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        })
}

fn validate(snapshot: &SourceSnapshot) -> Result<(), Error> {
    let m = &snapshot.manifest;
    if m.version != 1
        || m.api_version != 1
        || !token(&m.name)
        || m.modules.is_empty()
        || m.modules.len() > 64
        || m.parameters.len() > 64
        || !m.parameters.keys().all(|key| token(key))
        || !m.parameters.values().all(|ty| match ty {
            ParameterType::Text { max_bytes } => *max_bytes <= 16_384,
            ParameterType::Integer { min, max } => {
                min <= max
                    && *min >= -(crate::protocol::MAX_SAFE_INTEGER as i64)
                    && *max <= crate::protocol::MAX_SAFE_INTEGER as i64
            }
            _ => true,
        })
        || serde_json::to_vec(m).map_err(|_| Error::Snapshot)?.len() > 65_536
    {
        return Err(Error::Snapshot);
    }
    let paths: BTreeSet<_> = m.modules.iter().collect();
    let folded: BTreeSet<_> = m.modules.iter().map(|p| p.to_ascii_lowercase()).collect();
    if paths.len() != m.modules.len()
        || folded.len() != m.modules.len()
        || m.modules.windows(2).any(|p| p[0] >= p[1])
        || !m.modules.iter().all(|p| path(p))
        || !paths.contains(&m.entry)
        || snapshot.modules.keys().collect::<BTreeSet<_>>() != paths
        || snapshot.modules.values().map(String::len).sum::<usize>() > MAX_SOURCE_BYTES
        || snapshot.modules.values().any(|s| s.contains('\0'))
    {
        return Err(Error::Snapshot);
    }
    let mut graph = BTreeMap::new();
    for (name, source) in &snapshot.modules {
        let imports = static_imports(source)?;
        if imports.iter().any(|p| !snapshot.modules.contains_key(p)) {
            return Err(Error::Snapshot);
        }
        graph.insert(name.clone(), imports);
    }
    let mut seen = BTreeSet::new();
    let mut pending = vec![m.entry.clone()];
    while let Some(name) = pending.pop() {
        if seen.insert(name.clone()) {
            pending.extend(graph[&name].iter().cloned());
        }
    }
    if seen.len() != snapshot.modules.len() {
        return Err(Error::Snapshot);
    }
    Ok(())
}

/// Parse actual syntax, not a regexp/string scan (comments, templates and
/// escaped identifiers must not bypass the dynamic-import prohibition).
fn static_imports(source: &str) -> Result<BTreeSet<String>, Error> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_javascript::LANGUAGE.into())
        .map_err(|_| Error::Snapshot)?;
    let tree = parser.parse(source, None).ok_or(Error::Snapshot)?;
    if tree.root_node().has_error() {
        return Err(Error::Snapshot);
    }
    let mut imports = BTreeSet::new();
    let mut cursor = tree.walk();
    loop {
        let node = cursor.node();
        if node.kind() == "await_expression" {
            let mut parent = node.parent();
            let mut function = false;
            while let Some(p) = parent {
                if p.kind().contains("function")
                    || (p.kind() == "method_definition"
                        && p.child_by_field_name("body").is_some_and(|body| {
                            body.start_byte() <= node.start_byte()
                                && node.end_byte() <= body.end_byte()
                        }))
                {
                    function = true;
                    break;
                }
                parent = p.parent();
            }
            if !function {
                return Err(Error::Snapshot);
            }
        }
        if node.kind() == "import" && node.parent().is_some_and(|p| p.kind() == "call_expression") {
            return Err(Error::Snapshot);
        }
        if matches!(node.kind(), "import_statement" | "export_statement") {
            if let Some(value) = node.child_by_field_name("source") {
                let raw = &source[value.byte_range()];
                // Deliberately disallow escaped module names and all aliases.
                let p = raw
                    .get(1..raw.len().saturating_sub(1))
                    .ok_or(Error::Snapshot)?;
                if !path(p) {
                    return Err(Error::Snapshot);
                }
                imports.insert(p.to_owned());
            }
            if node.kind() == "import_statement" && node.child_by_field_name("source").is_none() {
                return Err(Error::Snapshot);
            }
            if (0..node.named_child_count()).any(|i| {
                node.named_child(i as u32)
                    .is_some_and(|n| n.kind() == "import_attribute")
            }) {
                return Err(Error::Snapshot);
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return Ok(imports);
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn fixture(source: &str) -> ApprovedBundle {
    ApprovedBundle::new(SourceSnapshot {
        manifest: Manifest {
            version: 1,
            api_version: 1,
            name: "test".into(),
            entry: "main.js".into(),
            modules: vec!["main.js".into()],
            parameters: BTreeMap::new(),
            capabilities: BTreeSet::new(),
        },
        modules: BTreeMap::from([("main.js".into(), source.into())]),
    })
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_covers_manifest_schema_sources_and_dependencies() {
        let a = fixture("export function main(){return 1}");
        let mut b = a.snapshot().clone();
        b.manifest.capabilities.insert(Capability::SendText);
        assert_ne!(a.digest(), ApprovedBundle::new(b).unwrap().digest());
        let b = fixture("export function main(){return 2}");
        assert_ne!(a.digest(), b.digest());
    }

    #[test]
    fn rejects_dynamic_import_aliases_extra_missing_and_case_collisions() {
        for source in [
            "export const x = import('main.js')",
            "export const x = import /* gap */ ('main.js')",
            "export const x = `${import('main.js')}`",
            "import x from '../main.js'; export {x}",
            "import x from 'https://x/a.js'; export {x}",
            "export * from 'missing.js'",
            "export const x = await Promise.resolve(1)",
        ] {
            let mut s = fixture("export const x=1").snapshot().clone();
            s.modules.insert("main.js".into(), source.into());
            assert!(ApprovedBundle::new(s).is_err(), "{source}");
        }
        let mut s = fixture("export const x=1").snapshot().clone();
        s.modules
            .insert("extra.js".into(), "export const x=2".into());
        assert!(ApprovedBundle::new(s.clone()).is_err());
        s.manifest.modules.insert(0, "extra.js".into());
        assert!(ApprovedBundle::new(s).is_err());
        let mut s = fixture("export const x=1").snapshot().clone();
        s.manifest.modules.insert(0, "Main.js".into());
        s.modules
            .insert("Main.js".into(), "export const x=2".into());
        assert!(ApprovedBundle::new(s).is_err());
        assert!(static_imports("const text = 'import(\"x\")'; // import('a')").is_ok());
    }

    #[test]
    fn async_methods_are_not_top_level_await() {
        assert!(
            static_imports("export class C { async method() { await Promise.resolve(); } }")
                .is_ok()
        );
        assert!(static_imports(
            "export const o = { async method() { await Promise.resolve(); } };"
        )
        .is_ok());
        assert!(static_imports("class C { [await Promise.resolve('x')]() {} }").is_err());
    }

    #[test]
    fn strict_parameters_and_unknown_fields() {
        assert!(serde_json::from_str::<SourceSnapshot>(
            r#"{"manifest":{},"modules":{},"unknown":true}"#
        )
        .is_err());
        let bundle = fixture("export const x=1");
        let json = serde_json::to_string(bundle.snapshot()).unwrap();
        let duplicate = json.replace(
            r#""modules":{"main.js":"export const x=1"}"#,
            r#""modules":{"main.js":"export const x=1","main.js":"export const x=2"}"#,
        );
        assert_ne!(json, duplicate);
        assert!(serde_json::from_str::<SourceSnapshot>(&duplicate).is_err());
        let b = fixture("export function main(){}");
        assert!(b
            .validate_parameters(&BTreeMap::from([(
                "extra".into(),
                ParameterValue::Boolean(true)
            )]))
            .is_err());
    }

    #[test]
    fn target_parameters_require_exact_safe_generation_bindings() {
        let mut snapshot = fixture("export function main(){}").snapshot().clone();
        snapshot
            .manifest
            .parameters
            .insert("first".into(), ParameterType::Target {});
        snapshot
            .manifest
            .parameters
            .insert("second".into(), ParameterType::Target {});
        let bundle = ApprovedBundle::new(snapshot).unwrap();
        let values = BTreeMap::from([
            ("first".into(), ParameterValue::Target("selected".into())),
            ("second".into(), ParameterValue::Target("selected".into())),
        ]);
        let bound = bundle
            .bind_parameters(values.clone(), BTreeMap::from([("selected".into(), 42)]))
            .unwrap();
        assert_eq!(
            bound.target("selected"),
            Some(crate::protocol::Target {
                id: "selected".into(),
                generation: 42,
            })
        );
        assert!(bundle
            .bind_parameters(values.clone(), BTreeMap::new())
            .is_err());
        assert!(bundle
            .bind_parameters(
                values.clone(),
                BTreeMap::from([("selected".into(), 42), ("extra".into(), 43)]),
            )
            .is_err());
        assert!(bundle
            .bind_parameters(
                values,
                BTreeMap::from([("selected".into(), crate::protocol::MAX_SAFE_INTEGER + 1)]),
            )
            .is_err());
        let mut changed = bundle.snapshot().clone();
        changed
            .modules
            .insert("main.js".into(), "export function main(){return 1}".into());
        let changed = ApprovedBundle::new(changed).unwrap();
        assert!(bound.validate(&changed).is_err());
        assert!(serde_json::from_str::<BoundParameters>(
            r#"{"bundleDigest":"x","values":{},"targetGenerations":{"x":1,"x":2}}"#
        )
        .is_err());
    }
}
