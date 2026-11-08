//! CLIXML decoder.
//!
//! Tolerant: unknown elements are skipped; a dangling `<Ref>` decodes to
//! [`PsValue::Null`]; leading whitespace / BOM / `<Objs>` wrappers are
//! accepted transparently.

use std::collections::HashMap;

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use super::{PsObject, PsValue};
use crate::error::{PsrpError, Result};

/// Parse a CLIXML fragment into a sequence of top-level values.
///
/// A single CLIXML body may contain several siblings (a PSRP `PipelineOutput`
/// message, for example, is one `<Obj>` at the top level — but a
/// `SessionCapability` message consists of an `<Obj>` with a `<MS>` of
/// primitive siblings, which this parser also handles).
pub fn parse_clixml(xml: &str) -> Result<Vec<PsValue>> {
    parse_clixml_with_budget(xml, DecodeBudget::default())
}

/// Bounded CLIXML decoding limits.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct DecodeBudget {
    pub max_input_bytes: usize,
    pub max_top_level_values: usize,
    pub max_nodes: usize,
    pub max_string_bytes: usize,
    pub max_byte_array_bytes: usize,
    pub max_collection_items: usize,
    pub max_dict_entries: usize,
    pub max_references: usize,
    pub max_reference_expansions: usize,
    pub max_type_names: usize,
    pub max_type_name_bytes: usize,
}

impl Default for DecodeBudget {
    fn default() -> Self {
        Self {
            max_input_bytes: 256 * 1024,
            max_top_level_values: 256,
            max_nodes: 8_192,
            max_string_bytes: 1_048_576,
            max_byte_array_bytes: 1_048_576,
            max_collection_items: 16_384,
            max_dict_entries: 4_096,
            max_references: 1_024,
            max_reference_expansions: 4_096,
            max_type_names: 2_048,
            max_type_name_bytes: 262_144,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ValueCost {
    nodes: usize,
    string_bytes: usize,
    byte_array_bytes: usize,
    collection_items: usize,
    dict_entries: usize,
    type_names: usize,
    type_name_bytes: usize,
}

#[derive(Default)]
struct BudgetTracker {
    limits: DecodeBudget,
    top_level_values: usize,
    nodes: usize,
    string_bytes: usize,
    byte_array_bytes: usize,
    collection_items: usize,
    dict_entries: usize,
    references: usize,
    reference_expansions: usize,
    type_names: usize,
    type_name_bytes: usize,
}

impl BudgetTracker {
    fn new(limits: DecodeBudget) -> Self {
        Self {
            limits,
            ..Self::default()
        }
    }

    fn add(kind: &str, current: &mut usize, amount: usize, limit: usize) -> Result<()> {
        let next = current
            .checked_add(amount)
            .ok_or_else(|| PsrpError::clixml(format!("CLIXML {kind} budget overflow")))?;
        if next > limit {
            return Err(PsrpError::clixml(format!(
                "CLIXML {kind} budget exceeded ({next} > {limit})"
            )));
        }
        *current = next;
        Ok(())
    }

    fn note_top_level_value(&mut self) -> Result<()> {
        Self::add(
            "top-level value",
            &mut self.top_level_values,
            1,
            self.limits.max_top_level_values,
        )
    }

    fn note_node(&mut self) -> Result<()> {
        Self::add("node", &mut self.nodes, 1, self.limits.max_nodes)
    }

    fn note_string(&mut self, bytes: usize) -> Result<()> {
        Self::add(
            "string bytes",
            &mut self.string_bytes,
            bytes,
            self.limits.max_string_bytes,
        )
    }

    fn note_byte_array(&mut self, bytes: usize) -> Result<()> {
        Self::add(
            "byte-array bytes",
            &mut self.byte_array_bytes,
            bytes,
            self.limits.max_byte_array_bytes,
        )
    }

    fn note_collection_item(&mut self) -> Result<()> {
        Self::add(
            "collection item",
            &mut self.collection_items,
            1,
            self.limits.max_collection_items,
        )
    }

    fn note_dict_entry(&mut self) -> Result<()> {
        Self::add(
            "dictionary entry",
            &mut self.dict_entries,
            1,
            self.limits.max_dict_entries,
        )
    }

    fn note_reference(&mut self) -> Result<()> {
        Self::add(
            "reference",
            &mut self.references,
            1,
            self.limits.max_references,
        )
    }

    fn note_reference_expansion(&mut self) -> Result<()> {
        Self::add(
            "reference expansion",
            &mut self.reference_expansions,
            1,
            self.limits.max_reference_expansions,
        )
    }

    fn note_type_name(&mut self, bytes: usize) -> Result<()> {
        Self::add(
            "type name",
            &mut self.type_names,
            1,
            self.limits.max_type_names,
        )?;
        Self::add(
            "type-name bytes",
            &mut self.type_name_bytes,
            bytes,
            self.limits.max_type_name_bytes,
        )
    }

    fn note_value_clone(&mut self, cost: ValueCost) -> Result<()> {
        Self::add("node", &mut self.nodes, cost.nodes, self.limits.max_nodes)?;
        Self::add(
            "string bytes",
            &mut self.string_bytes,
            cost.string_bytes,
            self.limits.max_string_bytes,
        )?;
        Self::add(
            "byte-array bytes",
            &mut self.byte_array_bytes,
            cost.byte_array_bytes,
            self.limits.max_byte_array_bytes,
        )?;
        Self::add(
            "collection item",
            &mut self.collection_items,
            cost.collection_items,
            self.limits.max_collection_items,
        )?;
        Self::add(
            "dictionary entry",
            &mut self.dict_entries,
            cost.dict_entries,
            self.limits.max_dict_entries,
        )?;
        Self::add(
            "type name",
            &mut self.type_names,
            cost.type_names,
            self.limits.max_type_names,
        )?;
        Self::add(
            "type-name bytes",
            &mut self.type_name_bytes,
            cost.type_name_bytes,
            self.limits.max_type_name_bytes,
        )
    }
}

#[derive(Clone)]
struct StoredValue {
    value: PsValue,
    cost: ValueCost,
}

#[derive(Clone)]
struct StoredTypeNames {
    names: Vec<String>,
    bytes: usize,
}

/// Parse CLIXML using explicit bounds.
pub fn parse_clixml_with_budget(xml: &str, budget: DecodeBudget) -> Result<Vec<PsValue>> {
    let cleaned = xml.trim_start_matches('\u{FEFF}').trim_start();
    if cleaned.len() > budget.max_input_bytes {
        return Err(PsrpError::clixml(format!(
            "CLIXML input exceeds {} bytes",
            budget.max_input_bytes
        )));
    }
    let mut reader = Reader::from_str(cleaned);
    reader.config_mut().trim_text(false);

    let mut state = DecoderState::new(budget);
    let mut out: Vec<PsValue> = Vec::new();
    let mut buf = Vec::new();

    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| PsrpError::clixml(e.to_string()))?
        {
            Event::Start(e) => {
                if let Some(value) = parse_element(&mut reader, &e, &mut state)? {
                    state.budget.note_top_level_value()?;
                    out.push(value);
                }
            }
            Event::Empty(e) => {
                if let Some(value) = parse_empty(&e, &mut state)? {
                    state.budget.note_top_level_value()?;
                    out.push(value);
                } else if e.name().as_ref() == "Ref" {
                    let rid = ref_ref_id_attr(&e)?;
                    let value = state.clone_reference(rid.as_deref())?;
                    state.budget.note_top_level_value()?;
                    out.push(value);
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(out)
}

/// Maximum `<Obj>` / `<LST>` / `<DCT>` nesting accepted from the wire.
///
/// The parser is recursive, and the document comes from the remote host:
/// without a cap, ~10 KiB of `<Obj><MS>` repeated is enough to overflow
/// the stack and **abort the process** — a stack overflow is not a
/// catchable panic, so `forbid(unsafe_code)` buys nothing here.
///
/// 64 was measured as the deepest level that still fits comfortably in
/// the 2 MiB stack of a Tokio worker thread in a debug build, and it is
/// an order of magnitude more than PowerShell's own serializer ever
/// emits (`Serialization.MaximumDepth` defaults to 1).
pub const MAX_NESTING_DEPTH: u32 = 32;

struct DecoderState {
    refs: HashMap<String, StoredValue>,
    type_names: HashMap<String, StoredTypeNames>,
    budget: BudgetTracker,
    /// Current nesting level, maintained by [`parse_element`].
    depth: u32,
}

impl DecoderState {
    fn new(limits: DecodeBudget) -> Self {
        Self {
            refs: HashMap::new(),
            type_names: HashMap::new(),
            budget: BudgetTracker::new(limits),
            depth: 0,
        }
    }

    fn clone_reference(&mut self, rid: Option<&str>) -> Result<PsValue> {
        let Some(rid) = rid else {
            return Ok(PsValue::Null);
        };
        let Some(stored) = self.refs.get(rid).cloned() else {
            return Ok(PsValue::Null);
        };
        self.budget.note_reference_expansion()?;
        self.budget.note_value_clone(stored.cost)?;
        Ok(stored.value)
    }

    fn clone_type_names(&mut self, rid: Option<&str>) -> Result<Option<Vec<String>>> {
        let Some(rid) = rid else {
            return Ok(None);
        };
        let Some(stored) = self.type_names.get(rid).cloned() else {
            return Ok(None);
        };
        self.budget.note_reference_expansion()?;
        self.budget.note_type_name(stored.bytes)?;
        Ok(Some(stored.names))
    }

    fn store_reference(&mut self, rid: String, value: &PsValue) -> Result<()> {
        let cost = measure_value_cost(value)?;
        self.budget.note_reference()?;
        self.budget.note_string(rid.len())?;
        self.budget.note_value_clone(cost)?;
        self.refs.insert(
            rid,
            StoredValue {
                value: value.clone(),
                cost,
            },
        );
        Ok(())
    }

    fn store_type_names(&mut self, rid: String, names: &[String]) -> Result<()> {
        let bytes = names.iter().map(String::len).sum::<usize>();
        self.budget.note_reference()?;
        self.budget.note_string(rid.len())?;
        for name in names {
            self.budget.note_type_name(name.len())?;
        }
        self.type_names.insert(
            rid,
            StoredTypeNames {
                names: names.to_vec(),
                bytes,
            },
        );
        Ok(())
    }
}

fn measure_value_cost(value: &PsValue) -> Result<ValueCost> {
    fn combine(a: &mut ValueCost, b: ValueCost) -> Result<()> {
        a.nodes = a
            .nodes
            .checked_add(b.nodes)
            .ok_or_else(|| PsrpError::clixml("CLIXML clone node cost overflow".to_string()))?;
        a.string_bytes = a
            .string_bytes
            .checked_add(b.string_bytes)
            .ok_or_else(|| PsrpError::clixml("CLIXML clone string cost overflow".to_string()))?;
        a.byte_array_bytes = a
            .byte_array_bytes
            .checked_add(b.byte_array_bytes)
            .ok_or_else(|| {
                PsrpError::clixml("CLIXML clone byte-array cost overflow".to_string())
            })?;
        a.collection_items = a
            .collection_items
            .checked_add(b.collection_items)
            .ok_or_else(|| PsrpError::clixml("CLIXML clone list cost overflow".to_string()))?;
        a.dict_entries = a
            .dict_entries
            .checked_add(b.dict_entries)
            .ok_or_else(|| PsrpError::clixml("CLIXML clone dict cost overflow".to_string()))?;
        a.type_names = a.type_names.checked_add(b.type_names).ok_or_else(|| {
            PsrpError::clixml("CLIXML clone type-name count overflow".to_string())
        })?;
        a.type_name_bytes = a
            .type_name_bytes
            .checked_add(b.type_name_bytes)
            .ok_or_else(|| {
                PsrpError::clixml("CLIXML clone type-name byte cost overflow".to_string())
            })?;
        Ok(())
    }

    let mut cost = ValueCost {
        nodes: 1,
        ..ValueCost::default()
    };
    match value {
        PsValue::Null
        | PsValue::Bool(_)
        | PsValue::I8(_)
        | PsValue::U8(_)
        | PsValue::I16(_)
        | PsValue::U16(_)
        | PsValue::I32(_)
        | PsValue::U32(_)
        | PsValue::I64(_)
        | PsValue::U64(_)
        | PsValue::F32(_)
        | PsValue::Double(_)
        | PsValue::Char(_)
        | PsValue::Guid(_) => {}
        PsValue::Decimal(s)
        | PsValue::String(s)
        | PsValue::DateTime(s)
        | PsValue::Duration(s)
        | PsValue::Version(s)
        | PsValue::Uri(s)
        | PsValue::Xml(s)
        | PsValue::ScriptBlock(s)
        | PsValue::SecureString(s) => {
            cost.string_bytes = s.len();
        }
        PsValue::Bytes(bytes) => {
            cost.byte_array_bytes = bytes.len();
        }
        PsValue::List(items) => {
            cost.collection_items = items.len();
            for item in items {
                combine(&mut cost, measure_value_cost(item)?)?;
            }
        }
        PsValue::Dict(entries) => {
            cost.dict_entries = entries.len();
            for (key, value) in entries {
                combine(&mut cost, measure_value_cost(key)?)?;
                combine(&mut cost, measure_value_cost(value)?)?;
            }
        }
        PsValue::Object(obj) => {
            cost.type_names = obj.type_names.len();
            cost.type_name_bytes = obj.type_names.iter().map(String::len).sum();
            if let Some(to_string) = &obj.to_string {
                cost.string_bytes =
                    cost.string_bytes
                        .checked_add(to_string.len())
                        .ok_or_else(|| {
                            PsrpError::clixml("CLIXML clone string cost overflow".to_string())
                        })?;
            }
            for (name, value) in &obj.properties {
                cost.string_bytes = cost.string_bytes.checked_add(name.len()).ok_or_else(|| {
                    PsrpError::clixml("CLIXML clone string cost overflow".to_string())
                })?;
                combine(&mut cost, measure_value_cost(value)?)?;
            }
        }
    }
    Ok(cost)
}

/// Read the `N="…"` property-name attribute.
///
/// The value is XML-unescaped and then run through the PowerShell
/// `_xHHHH_` decoder, mirroring exactly what `encode::escape` did on the
/// way out. Skipping either step corrupts any property name containing
/// `&`, `<`, `>`, `"`, `'` or a control character — and because the
/// re-encoder escapes the result again, the damage compounds on every
/// hop (`&lt;` becomes `&amp;lt;` becomes `&amp;amp;lt;` …).
fn name_attr(e: &BytesStart) -> Result<Option<String>> {
    for attr in e.attributes().flatten() {
        if attr.key.as_ref() == "N" {
            let unescaped = attr
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map_err(|err| PsrpError::clixml(err.to_string()))?;
            return Ok(Some(decode_pwsh_escapes(&unescaped)));
        }
    }
    Ok(None)
}

/// Read the `RefId="…"` attribute of an `<Obj>` / `<TN>`.
fn ref_id_attr(e: &BytesStart) -> Result<Option<String>> {
    for attr in e.attributes().flatten() {
        if attr.key.as_ref() == "RefId" {
            let unescaped = attr
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map_err(|err| PsrpError::clixml(err.to_string()))?;
            return Ok(Some(unescaped.into_owned()));
        }
    }
    Ok(None)
}

/// Read the `RefId="…"` attribute of a `<Ref>` / `<TNRef>`.
fn ref_ref_id_attr(e: &BytesStart) -> Result<Option<String>> {
    ref_id_attr(e)
}

fn parse_empty(e: &BytesStart, state: &mut DecoderState) -> Result<Option<PsValue>> {
    match e.name().as_ref() {
        "Nil" => {
            state.budget.note_node()?;
            Ok(Some(PsValue::Null))
        }
        "S" => {
            state.budget.note_node()?;
            Ok(Some(PsValue::String(String::new())))
        }
        "ToString" => Ok(None),
        _ => Ok(None),
    }
}

fn parse_int<T>(reader: &mut Reader<&[u8]>, closing: &str) -> Result<T>
where
    T: std::str::FromStr,
    <T as std::str::FromStr>::Err: std::fmt::Display,
{
    let text = read_text(reader, closing)?;
    text.trim()
        .parse::<T>()
        .map_err(|err| PsrpError::clixml(format!("{closing}: {err}")))
}

fn parse_float(s: &str) -> std::result::Result<f64, String> {
    match s {
        "NaN" => Ok(f64::NAN),
        "Infinity" => Ok(f64::INFINITY),
        "-Infinity" => Ok(f64::NEG_INFINITY),
        other => other.parse::<f64>().map_err(|e| e.to_string()),
    }
}

fn parse_element(
    reader: &mut Reader<&[u8]>,
    e: &BytesStart,
    state: &mut DecoderState,
) -> Result<Option<PsValue>> {
    if state.depth >= MAX_NESTING_DEPTH {
        return Err(PsrpError::clixml(format!(
            "CLIXML nested deeper than {MAX_NESTING_DEPTH} levels"
        )));
    }
    state.depth += 1;
    let result = parse_element_inner(reader, e, state);
    state.depth -= 1;
    result
}

fn parse_element_inner(
    reader: &mut Reader<&[u8]>,
    e: &BytesStart,
    state: &mut DecoderState,
) -> Result<Option<PsValue>> {
    let tag = e.name().as_ref().to_string();
    match tag.as_str() {
        "S" => {
            let text = read_text(reader, "S")?;
            state.budget.note_node()?;
            state.budget.note_string(text.len())?;
            Ok(Some(PsValue::String(text)))
        }
        "I32" => {
            let text = read_text(reader, "I32")?;
            let v = text
                .trim()
                .parse::<i32>()
                .map_err(|err| PsrpError::clixml(format!("I32: {err}")))?;
            state.budget.note_node()?;
            Ok(Some(PsValue::I32(v)))
        }
        "I64" => {
            let text = read_text(reader, "I64")?;
            let v = text
                .trim()
                .parse::<i64>()
                .map_err(|err| PsrpError::clixml(format!("I64: {err}")))?;
            state.budget.note_node()?;
            Ok(Some(PsValue::I64(v)))
        }
        "B" => {
            let text = read_text(reader, "B")?;
            let v = match text.trim().to_ascii_lowercase().as_str() {
                "true" | "1" => true,
                "false" | "0" => false,
                other => return Err(PsrpError::clixml(format!("B: bad bool '{other}'"))),
            };
            state.budget.note_node()?;
            Ok(Some(PsValue::Bool(v)))
        }
        "Db" => {
            let text = read_text(reader, "Db")?;
            let v =
                parse_float(text.trim()).map_err(|err| PsrpError::clixml(format!("Db: {err}")))?;
            state.budget.note_node()?;
            Ok(Some(PsValue::Double(v)))
        }
        "Sg" => {
            let text = read_text(reader, "Sg")?;
            let v =
                parse_float(text.trim()).map_err(|err| PsrpError::clixml(format!("Sg: {err}")))?;
            state.budget.note_node()?;
            Ok(Some(PsValue::F32(v as f32)))
        }
        "SB" => {
            state.budget.note_node()?;
            Ok(Some(PsValue::I8(parse_int(reader, "SB")?)))
        }
        "By" => {
            state.budget.note_node()?;
            Ok(Some(PsValue::U8(parse_int(reader, "By")?)))
        }
        "I16" => {
            state.budget.note_node()?;
            Ok(Some(PsValue::I16(parse_int(reader, "I16")?)))
        }
        "U16" => {
            state.budget.note_node()?;
            Ok(Some(PsValue::U16(parse_int(reader, "U16")?)))
        }
        "U32" => {
            state.budget.note_node()?;
            Ok(Some(PsValue::U32(parse_int(reader, "U32")?)))
        }
        "U64" => {
            state.budget.note_node()?;
            Ok(Some(PsValue::U64(parse_int(reader, "U64")?)))
        }
        "D" => {
            let text = read_text(reader, "D")?;
            state.budget.note_node()?;
            state.budget.note_string(text.len())?;
            Ok(Some(PsValue::Decimal(text.trim().to_string())))
        }
        "C" => {
            let text = read_text(reader, "C")?;
            let code: u32 = text
                .trim()
                .parse()
                .map_err(|err| PsrpError::clixml(format!("C: {err}")))?;
            let ch = char::from_u32(code)
                .ok_or_else(|| PsrpError::clixml(format!("C: invalid code point {code}")))?;
            state.budget.note_node()?;
            Ok(Some(PsValue::Char(ch)))
        }
        "BA" => {
            let text = read_text(reader, "BA")?;
            let bytes = super::encode::base64_decode(text.trim())
                .ok_or_else(|| PsrpError::clixml("BA: invalid base64".to_string()))?;
            state.budget.note_node()?;
            state.budget.note_byte_array(bytes.len())?;
            Ok(Some(PsValue::Bytes(bytes)))
        }
        "DT" => {
            let text = read_text(reader, "DT")?;
            state.budget.note_node()?;
            state.budget.note_string(text.len())?;
            Ok(Some(PsValue::DateTime(text)))
        }
        "TS" => {
            let text = read_text(reader, "TS")?;
            state.budget.note_node()?;
            state.budget.note_string(text.len())?;
            Ok(Some(PsValue::Duration(text)))
        }
        "G" => {
            let text = read_text(reader, "G")?;
            let uuid = uuid::Uuid::parse_str(text.trim())
                .map_err(|err| PsrpError::clixml(format!("G: {err}")))?;
            state.budget.note_node()?;
            Ok(Some(PsValue::Guid(uuid)))
        }
        "Version" => {
            let text = read_text(reader, "Version")?;
            state.budget.note_node()?;
            state.budget.note_string(text.len())?;
            Ok(Some(PsValue::Version(text)))
        }
        "URI" => {
            let text = read_text(reader, "URI")?;
            state.budget.note_node()?;
            state.budget.note_string(text.len())?;
            Ok(Some(PsValue::Uri(text)))
        }
        "XD" => {
            let text = read_text(reader, "XD")?;
            state.budget.note_node()?;
            state.budget.note_string(text.len())?;
            Ok(Some(PsValue::Xml(text)))
        }
        "SCT" => {
            let text = read_text(reader, "SCT")?;
            state.budget.note_node()?;
            state.budget.note_string(text.len())?;
            Ok(Some(PsValue::ScriptBlock(text)))
        }
        "SS" => {
            let text = read_text(reader, "SS")?;
            state.budget.note_node()?;
            state.budget.note_string(text.len())?;
            Ok(Some(PsValue::SecureString(text)))
        }
        "Obj" => {
            state.budget.note_node()?;
            let ref_id = ref_id_attr(e)?;
            let obj = parse_obj_body(reader, state)?;
            let value = PsValue::Object(obj);
            if let Some(rid) = ref_id {
                state.store_reference(rid, &value)?;
            }
            Ok(Some(value))
        }
        "Ref" => {
            let rid = ref_ref_id_attr(e)?;
            skip_to_end(reader)?;
            Ok(Some(state.clone_reference(rid.as_deref())?))
        }
        _ => {
            skip_to_end(reader)?;
            Ok(None)
        }
    }
}

fn parse_obj_body(reader: &mut Reader<&[u8]>, state: &mut DecoderState) -> Result<PsObject> {
    let mut obj = PsObject::new();
    let mut buf = Vec::new();
    let mut embedded: Option<PsValue> = None;
    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| PsrpError::clixml(e.to_string()))?
        {
            Event::Start(e) => match e.name().as_ref() {
                "MS" | "Props" => {
                    parse_member_set(reader, state, &mut obj, e.name().as_ref().to_string())?;
                }
                "TN" => {
                    let rid = ref_id_attr(&e)?;
                    let names = parse_type_names(reader, state)?;
                    if let Some(rid) = rid {
                        state.store_type_names(rid, &names)?;
                    }
                    obj.type_names = names;
                }
                "TNRef" => {
                    let rid = ref_ref_id_attr(&e)?;
                    skip_to_end(reader)?;
                    if let Some(names) = state.clone_type_names(rid.as_deref())? {
                        obj.type_names = names;
                    }
                }
                "LST" | "IE" | "QUE" | "STK" => {
                    let items = parse_list(reader, state, e.name().as_ref().to_string())?;
                    embedded = Some(PsValue::List(items));
                }
                "DCT" => {
                    let entries = parse_dict(reader, state)?;
                    embedded = Some(PsValue::Dict(entries));
                }
                _ => skip_to_end(reader)?,
            },
            Event::Empty(e) => match e.name().as_ref() {
                "TNRef" => {
                    let rid = ref_ref_id_attr(&e)?;
                    if let Some(names) = state.clone_type_names(rid.as_deref())? {
                        obj.type_names = names;
                    }
                }
                "ToString" | "Nil" => {}
                _ => {}
            },
            Event::End(e) if e.name().as_ref() == "Obj" => break,
            Event::Eof => {
                return Err(PsrpError::clixml("unexpected EOF inside <Obj>"));
            }
            _ => {}
        }
        buf.clear();
    }

    if let Some(v) = embedded
        && obj.properties.is_empty()
    {
        obj.properties.insert("_value".into(), v);
    }

    Ok(obj)
}

fn parse_member_set(
    reader: &mut Reader<&[u8]>,
    state: &mut DecoderState,
    obj: &mut PsObject,
    closing_tag: String,
) -> Result<()> {
    let mut buf = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| PsrpError::clixml(e.to_string()))?
        {
            Event::Start(e) => {
                let name = name_attr(&e)?.unwrap_or_default();
                state.budget.note_string(name.len())?;
                if let Some(v) = parse_element(reader, &e, state)? {
                    obj.properties.insert(name, v);
                }
            }
            Event::Empty(e) => match e.name().as_ref() {
                "Nil" => {
                    if let Some(name) = name_attr(&e)? {
                        state.budget.note_string(name.len())?;
                        state.budget.note_node()?;
                        obj.properties.insert(name, PsValue::Null);
                    }
                }
                "Ref" => {
                    let name = name_attr(&e)?.unwrap_or_default();
                    state.budget.note_string(name.len())?;
                    let rid = ref_ref_id_attr(&e)?;
                    let value = state.clone_reference(rid.as_deref())?;
                    obj.properties.insert(name, value);
                }
                _ => {}
            },
            Event::End(e) if e.name().as_ref() == closing_tag.as_str() => break,
            Event::Eof => return Err(PsrpError::clixml("EOF inside member set")),
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

fn parse_list(
    reader: &mut Reader<&[u8]>,
    state: &mut DecoderState,
    closing_tag: String,
) -> Result<Vec<PsValue>> {
    state.budget.note_node()?;
    let mut items = Vec::new();
    let mut buf = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| PsrpError::clixml(e.to_string()))?
        {
            Event::Start(e) => {
                if let Some(v) = parse_element(reader, &e, state)? {
                    state.budget.note_collection_item()?;
                    items.push(v);
                }
            }
            Event::Empty(e) => {
                if let Some(v) = parse_empty(&e, state)? {
                    state.budget.note_collection_item()?;
                    items.push(v);
                }
            }
            Event::End(e) if e.name().as_ref() == closing_tag.as_str() => break,
            Event::Eof => return Err(PsrpError::clixml("EOF inside list")),
            _ => {}
        }
        buf.clear();
    }
    Ok(items)
}

fn parse_dict(
    reader: &mut Reader<&[u8]>,
    state: &mut DecoderState,
) -> Result<Vec<(PsValue, PsValue)>> {
    state.budget.note_node()?;
    let mut entries = Vec::new();
    let mut buf = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| PsrpError::clixml(e.to_string()))?
        {
            Event::Start(e) if e.name().as_ref() == "En" => {
                let (k, v) = parse_dict_entry(reader, state)?;
                state.budget.note_dict_entry()?;
                entries.push((k, v));
            }
            Event::End(e) if e.name().as_ref() == "DCT" => break,
            Event::Eof => return Err(PsrpError::clixml("EOF inside <DCT>")),
            _ => {}
        }
        buf.clear();
    }
    Ok(entries)
}

fn parse_dict_entry(
    reader: &mut Reader<&[u8]>,
    state: &mut DecoderState,
) -> Result<(PsValue, PsValue)> {
    let mut key = PsValue::Null;
    let mut val = PsValue::Null;
    let mut buf = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| PsrpError::clixml(e.to_string()))?
        {
            Event::Start(e) => {
                let name = name_attr(&e)?.unwrap_or_default();
                if let Some(v) = parse_element(reader, &e, state)? {
                    match name.as_str() {
                        "Key" => key = v,
                        "Value" => val = v,
                        _ => {}
                    }
                }
            }
            Event::Empty(e) if e.name().as_ref() == "Nil" => {
                let name = name_attr(&e)?.unwrap_or_default();
                state.budget.note_node()?;
                match name.as_str() {
                    "Key" => key = PsValue::Null,
                    "Value" => val = PsValue::Null,
                    _ => {}
                }
            }
            Event::End(e) if e.name().as_ref() == "En" => break,
            Event::Eof => return Err(PsrpError::clixml("EOF inside <En>")),
            _ => {}
        }
        buf.clear();
    }
    Ok((key, val))
}

fn parse_type_names(reader: &mut Reader<&[u8]>, state: &mut DecoderState) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut buf = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| PsrpError::clixml(e.to_string()))?
        {
            Event::Start(e) if e.name().as_ref() == "T" => {
                let text = read_text(reader, "T")?;
                state.budget.note_type_name(text.len())?;
                out.push(text);
            }
            Event::End(e) if e.name().as_ref() == "TN" => break,
            Event::Eof => return Err(PsrpError::clixml("EOF inside <TN>")),
            _ => {}
        }
        buf.clear();
    }
    Ok(out)
}

fn read_text(reader: &mut Reader<&[u8]>, closing: &str) -> Result<String> {
    let mut out = String::new();
    let mut buf = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| PsrpError::clixml(e.to_string()))?
        {
            Event::Text(t) => {
                out.push_str(&t);
            }
            Event::GeneralRef(r) => {
                if let Some(c) = r
                    .resolve_char_ref()
                    .map_err(|e| PsrpError::clixml(e.to_string()))?
                {
                    out.push(c);
                } else {
                    let name: &str = r.as_ref();
                    match quick_xml::escape::resolve_predefined_entity(name) {
                        Some(s) => out.push_str(s),
                        None => {
                            return Err(PsrpError::clixml(format!(
                                "unknown entity reference &{name};"
                            )));
                        }
                    }
                }
            }
            Event::CData(c) => {
                out.push_str(c.as_ref());
            }
            Event::End(e) if e.name().as_ref() == closing => break,
            Event::Eof => {
                return Err(PsrpError::clixml(format!("EOF reading <{closing}>")));
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(decode_pwsh_escapes(&out))
}

fn skip_to_end(reader: &mut Reader<&[u8]>) -> Result<()> {
    let mut depth: usize = 1;
    let mut buf = Vec::new();
    loop {
        match reader
            .read_event_into(&mut buf)
            .map_err(|e| PsrpError::clixml(e.to_string()))?
        {
            Event::Start(_) => depth = depth.saturating_add(1),
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    Ok(())
}

fn decode_pwsh_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    let len = bytes.len();
    while i < len {
        if i + 7 <= len
            && bytes[i] == b'_'
            && bytes[i + 1] == b'x'
            && bytes[i + 6] == b'_'
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 2..i + 6])
            && let Ok(code) = u32::from_str_radix(hex, 16)
            && let Some(c) = char::from_u32(code)
        {
            out.push(c);
            i += 7;
            continue;
        }
        let ch = s[i..].chars().next().expect("in-bounds char");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}
#[cfg(test)]
mod tests {
    /// Regression (found while fuzzing `clixml_decoder`): the parser is
    /// recursive and the document is attacker-controlled, so ~10 KiB of
    /// `<Obj><MS>` used to overflow the stack and abort the process.
    #[test]
    fn deep_nesting_is_rejected_not_fatal() {
        use super::super::parse_clixml;
        use super::MAX_NESTING_DEPTH;

        let deep = |n: usize| "<Obj><MS>".repeat(n) + "<Nil/>" + &"</MS></Obj>".repeat(n);

        // Comfortably inside the limit: still parses.
        parse_clixml(&deep(MAX_NESTING_DEPTH as usize / 2)).expect("shallow document");

        // Past it: a structured error, never a crash.
        let err = parse_clixml(&deep(MAX_NESTING_DEPTH as usize + 5))
            .expect_err("over-deep document must be rejected");
        assert!(
            err.to_string().contains("nested deeper"),
            "unexpected error: {err}"
        );

        // And a substantially deeper document than any legitimate serializer emits.
        assert!(parse_clixml(&deep(MAX_NESTING_DEPTH as usize + 64)).is_err());
    }

    // Regression (found by the `clixml_encode_decode` fuzz target):
    // attribute values used to be taken raw, so a property name holding
    // an XML metacharacter came back still escaped — and the encoder
    // then escaped it again on the next hop.
    #[test]
    fn property_names_are_xml_unescaped() {
        use super::super::{PsObject, PsValue, parse_clixml, to_clixml};

        let value = PsValue::Object(
            PsObject::new()
                .with("a<b", PsValue::I32(1))
                .with("x&y", PsValue::I32(2))
                .with("q\"r", PsValue::I32(3)),
        );

        let xml = to_clixml(&value);
        let decoded = parse_clixml(&xml).expect("decodes");
        assert_eq!(decoded[0], value, "property names were mangled");

        // And the damage must not compound across hops.
        let again = parse_clixml(&to_clixml(&decoded[0])).expect("decodes");
        assert_eq!(again[0], value);
    }

    #[test]
    fn property_names_decode_pwsh_escapes() {
        use super::super::{PsObject, PsValue, parse_clixml, to_clixml};

        // `escape` turns control characters in a name into `_xHHHH_`;
        // the decoder has to turn them back.
        let value = PsValue::Object(PsObject::new().with("a\u{1}b", PsValue::Null));
        let xml = to_clixml(&value);
        assert!(xml.contains("_x0001_"), "encoder did not escape: {xml}");
        let decoded = parse_clixml(&xml).expect("decodes");
        assert_eq!(decoded[0], value);
    }

    #[test]
    fn reference_expansion_budget_rejects_clone_amplification() {
        let xml = concat!(
            "<Obj RefId=\"0\"><MS><S N=\"Name\">alpha</S></MS></Obj>",
            "<Ref RefId=\"0\"/>",
            "<Ref RefId=\"0\"/>",
            "<Ref RefId=\"0\"/>",
        );
        let err = parse_clixml_with_budget(
            xml,
            DecodeBudget {
                max_string_bytes: 20,
                ..DecodeBudget::default()
            },
        )
        .expect_err("budget should reject repeated deep clones");
        assert!(err.to_string().contains("budget exceeded"));
    }

    #[test]
    fn reference_expansion_within_budget_preserves_typed_values() {
        let xml = concat!(
            "<Obj RefId=\"0\"><MS><I32 N=\"Value\">7</I32></MS></Obj>",
            "<Ref RefId=\"0\"/>",
        );
        let values = parse_clixml_with_budget(
            xml,
            DecodeBudget {
                max_string_bytes: 64,
                max_nodes: 32,
                ..DecodeBudget::default()
            },
        )
        .expect("decode within budget");
        assert_eq!(values.len(), 2);
        assert_eq!(values[0], values[1]);
    }

    use super::super::encode::to_clixml;
    use super::*;

    #[test]
    fn primitives() {
        let cases = vec![
            ("<S>hi</S>", PsValue::String("hi".into())),
            ("<I32>-5</I32>", PsValue::I32(-5)),
            ("<I64>99</I64>", PsValue::I64(99)),
            ("<B>true</B>", PsValue::Bool(true)),
            ("<B>false</B>", PsValue::Bool(false)),
            ("<Db>1.5</Db>", PsValue::Double(1.5)),
            ("<Nil/>", PsValue::Null),
        ];
        for (xml, expected) in cases {
            let got = parse_clixml(xml).unwrap();
            assert_eq!(got.len(), 1, "{xml}");
            assert_eq!(got[0], expected, "{xml}");
        }
    }

    #[test]
    fn double_special_values() {
        assert!(
            matches!(parse_clixml("<Db>NaN</Db>").unwrap()[0], PsValue::Double(v) if v.is_nan())
        );
        assert!(matches!(
            parse_clixml("<Db>Infinity</Db>").unwrap()[0],
            PsValue::Double(v) if v.is_infinite() && v.is_sign_positive()
        ));
        assert!(matches!(
            parse_clixml("<Db>-Infinity</Db>").unwrap()[0],
            PsValue::Double(v) if v.is_infinite() && v.is_sign_negative()
        ));
    }

    #[test]
    fn bool_accepts_1_and_0() {
        assert_eq!(parse_clixml("<B>1</B>").unwrap()[0], PsValue::Bool(true));
        assert_eq!(parse_clixml("<B>0</B>").unwrap()[0], PsValue::Bool(false));
    }

    #[test]
    fn bad_bool_errors() {
        assert!(parse_clixml("<B>maybe</B>").is_err());
    }

    #[test]
    fn bad_int_errors() {
        assert!(parse_clixml("<I32>not-a-number</I32>").is_err());
        assert!(parse_clixml("<I64>xx</I64>").is_err());
        assert!(parse_clixml("<Db>oops</Db>").is_err());
    }

    #[test]
    fn escapes_and_bom() {
        let xml = "\u{FEFF}  <S>&lt;hi&amp;&gt;</S>";
        let got = parse_clixml(xml).unwrap();
        assert_eq!(got[0], PsValue::String("<hi&>".into()));
    }

    #[test]
    fn pwsh_escape_decode() {
        let got = parse_clixml("<S>ab_x0001_cd</S>").unwrap();
        assert_eq!(got[0], PsValue::String("ab\u{0001}cd".into()));
    }

    #[test]
    fn object_with_member_set() {
        let xml = r#"<Obj RefId="0"><TN RefId="0"><T>System.Diagnostics.Process</T></TN><MS><S N="Name">svchost</S><I32 N="Id">42</I32><Nil N="Maybe"/></MS></Obj>"#;
        let got = parse_clixml(xml).unwrap();
        assert_eq!(got.len(), 1);
        let obj = match &got[0] {
            PsValue::Object(o) => o,
            _ => panic!("expected object"),
        };
        assert_eq!(
            obj.type_names,
            vec!["System.Diagnostics.Process".to_string()]
        );
        assert_eq!(obj.get("Name"), Some(&PsValue::String("svchost".into())));
        assert_eq!(obj.get("Id"), Some(&PsValue::I32(42)));
        assert_eq!(obj.get("Maybe"), Some(&PsValue::Null));
    }

    #[test]
    fn object_with_list_and_dict() {
        let xml = r#"<Obj RefId="0"><LST><I32>1</I32><I32>2</I32></LST></Obj>"#;
        let got = parse_clixml(xml).unwrap();
        let obj = match &got[0] {
            PsValue::Object(o) => o,
            _ => panic!("expected object"),
        };
        assert_eq!(
            obj.get("_value"),
            Some(&PsValue::List(vec![PsValue::I32(1), PsValue::I32(2)]))
        );
    }

    #[test]
    fn tnref_resolution() {
        // Outer object defines TN with RefId=0. Inner object references it via TNRef.
        let xml = r#"
          <Obj RefId="0"><TN RefId="0"><T>Foo</T></TN><MS><S N="k">v</S></MS></Obj>
          <Obj RefId="1"><TNRef RefId="0"/><MS><I32 N="n">7</I32></MS></Obj>
        "#;
        let got = parse_clixml(xml).unwrap();
        assert_eq!(got.len(), 2);
        if let PsValue::Object(o) = &got[1] {
            assert_eq!(o.type_names, vec!["Foo".to_string()]);
            assert_eq!(o.get("n"), Some(&PsValue::I32(7)));
        } else {
            panic!();
        }
    }

    #[test]
    fn ref_resolution() {
        let xml = r#"<Obj RefId="abc"><MS><S N="k">v</S></MS></Obj><Ref RefId="abc"/>"#;
        let got = parse_clixml(xml).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], got[1]);
    }

    #[test]
    fn dangling_ref_becomes_null() {
        let got = parse_clixml(r#"<Ref RefId="missing"/>"#).unwrap();
        assert_eq!(got[0], PsValue::Null);
    }

    #[test]
    fn unknown_elements_are_skipped() {
        let xml =
            r#"<Obj RefId="0"><UnknownThing><nested/></UnknownThing><MS><S N="k">v</S></MS></Obj>"#;
        let got = parse_clixml(xml).unwrap();
        if let PsValue::Object(o) = &got[0] {
            assert_eq!(o.get("k"), Some(&PsValue::String("v".into())));
        } else {
            panic!();
        }
    }

    #[test]
    fn roundtrip_complex_object() {
        let obj = PsObject {
            type_names: vec!["Foo".into(), "Bar".into()],
            to_string: None,
            properties: {
                let mut p = indexmap::IndexMap::new();
                p.insert("name".into(), PsValue::String("n".into()));
                p.insert("count".into(), PsValue::I32(3));
                p.insert("flag".into(), PsValue::Bool(true));
                p.insert("empty".into(), PsValue::Null);
                p.insert(
                    "tags".into(),
                    PsValue::List(vec![
                        PsValue::String("a".into()),
                        PsValue::String("b".into()),
                    ]),
                );
                p
            },
        };
        let xml = to_clixml(&PsValue::Object(obj.clone()));
        let got = parse_clixml(&xml).unwrap();
        let got_obj = match &got[0] {
            PsValue::Object(o) => o,
            _ => panic!(),
        };
        assert_eq!(got_obj.type_names, obj.type_names);
        assert_eq!(got_obj.get("name"), obj.properties.get("name"));
        assert_eq!(got_obj.get("count"), obj.properties.get("count"));
        assert_eq!(got_obj.get("flag"), obj.properties.get("flag"));
        assert_eq!(got_obj.get("empty"), Some(&PsValue::Null));
        // Embedded list comes back as an object whose _value is the list.
        if let Some(PsValue::Object(tags_obj)) = got_obj.get("tags") {
            assert_eq!(
                tags_obj.get("_value"),
                Some(&PsValue::List(vec![
                    PsValue::String("a".into()),
                    PsValue::String("b".into())
                ]))
            );
        } else {
            panic!("expected tags to be an object wrapping a list");
        }
    }

    #[test]
    fn cdata_section_decoded() {
        let got = parse_clixml("<S><![CDATA[hello & <world>]]></S>").unwrap();
        assert_eq!(got[0], PsValue::String("hello & <world>".into()));
    }

    #[test]
    fn top_level_ref_with_content_resolves() {
        // Non-self-closing `<Ref RefId="..">…</Ref>` at top level.
        let xml = r#"<Obj RefId="a"><MS><S N="k">v</S></MS></Obj><Ref RefId="a">ignored</Ref>"#;
        let got = parse_clixml(xml).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], got[1]);
    }

    #[test]
    fn empty_obj_body_produces_empty_object() {
        let got = parse_clixml("<Obj RefId=\"0\"></Obj>").unwrap();
        if let PsValue::Object(o) = &got[0] {
            assert!(o.properties.is_empty());
            assert!(o.type_names.is_empty());
        } else {
            panic!();
        }
    }

    #[test]
    fn props_element_is_treated_like_ms() {
        let xml = r#"<Obj RefId="0"><Props><S N="k">v</S></Props></Obj>"#;
        let got = parse_clixml(xml).unwrap();
        if let PsValue::Object(o) = &got[0] {
            assert_eq!(o.get("k"), Some(&PsValue::String("v".into())));
        } else {
            panic!();
        }
    }

    #[test]
    fn pwsh_escape_roundtrip() {
        assert_eq!(decode_pwsh_escapes("_x0041_BC"), "ABC");
        assert_eq!(decode_pwsh_escapes("no escapes here"), "no escapes here");
        assert_eq!(
            decode_pwsh_escapes("_xZZZZ_"),
            "_xZZZZ_",
            "invalid hex passes through"
        );
        assert_eq!(
            decode_pwsh_escapes("café"),
            "café",
            "multi-byte chars preserved"
        );
    }

    #[test]
    fn extended_primitives() {
        // SB (i8), By (u8), I16, U16, U32, U64
        let cases: Vec<(&str, PsValue)> = vec![
            ("<SB>-128</SB>", PsValue::I8(-128)),
            ("<By>255</By>", PsValue::U8(255)),
            ("<I16>-32000</I16>", PsValue::I16(-32000)),
            ("<U16>65535</U16>", PsValue::U16(65535)),
            ("<U32>4000000000</U32>", PsValue::U32(4_000_000_000)),
            ("<U64>18446744073709551615</U64>", PsValue::U64(u64::MAX)),
        ];
        for (xml, expected) in cases {
            let got = parse_clixml(xml).unwrap();
            assert_eq!(got.len(), 1, "{xml}");
            assert_eq!(got[0], expected, "{xml}");
        }
    }

    #[test]
    fn extended_primitive_errors() {
        assert!(parse_clixml("<SB>not_int</SB>").is_err());
        assert!(parse_clixml("<By>-1</By>").is_err());
        assert!(parse_clixml("<I16>99999</I16>").is_err());
        assert!(parse_clixml("<U16>-1</U16>").is_err());
        assert!(parse_clixml("<U32>-1</U32>").is_err());
        assert!(parse_clixml("<U64>not_a_number</U64>").is_err());
    }

    #[test]
    fn char_primitive() {
        let got = parse_clixml("<C>65</C>").unwrap();
        assert_eq!(got[0], PsValue::Char('A'));
    }

    #[test]
    fn char_invalid_code_point() {
        // 0xD800 is a surrogate, not a valid char
        assert!(parse_clixml("<C>55296</C>").is_err());
        // Not a number
        assert!(parse_clixml("<C>abc</C>").is_err());
    }

    #[test]
    fn base64_primitive() {
        // "aGVsbG8=" = "hello"
        let got = parse_clixml("<BA>aGVsbG8=</BA>").unwrap();
        assert_eq!(got[0], PsValue::Bytes(b"hello".to_vec()));
    }

    #[test]
    fn base64_invalid() {
        assert!(parse_clixml("<BA>!!!not-base64!!!</BA>").is_err());
    }

    #[test]
    fn guid_primitive() {
        let got = parse_clixml("<G>12345678-1234-1234-1234-123456789abc</G>").unwrap();
        if let PsValue::Guid(g) = &got[0] {
            assert_eq!(g.to_string(), "12345678-1234-1234-1234-123456789abc");
        } else {
            panic!("expected Guid");
        }
    }

    #[test]
    fn guid_invalid() {
        assert!(parse_clixml("<G>not-a-guid</G>").is_err());
    }

    #[test]
    fn datetime_and_duration() {
        let got = parse_clixml("<DT>2024-01-01T00:00:00Z</DT>").unwrap();
        assert_eq!(
            got[0],
            PsValue::DateTime("2024-01-01T00:00:00Z".to_string())
        );
        let got = parse_clixml("<TS>P1DT2H</TS>").unwrap();
        assert_eq!(got[0], PsValue::Duration("P1DT2H".to_string()));
    }

    #[test]
    fn version_uri_xml_scriptblock_securestring() {
        let cases = vec![
            ("<Version>5.1</Version>", PsValue::Version("5.1".into())),
            (
                "<URI>http://example.com</URI>",
                PsValue::Uri("http://example.com".into()),
            ),
            (
                "<XD>some xml data</XD>",
                PsValue::Xml("some xml data".into()),
            ),
            (
                "<SCT>Get-Process</SCT>",
                PsValue::ScriptBlock("Get-Process".into()),
            ),
            (
                "<SS>encrypted</SS>",
                PsValue::SecureString("encrypted".into()),
            ),
        ];
        for (xml, expected) in cases {
            let got = parse_clixml(xml).unwrap();
            assert_eq!(got.len(), 1, "{xml}");
            assert_eq!(got[0], expected, "{xml}");
        }
    }

    #[test]
    fn decimal_primitive() {
        let got = parse_clixml("<D>123.456</D>").unwrap();
        assert_eq!(got[0], PsValue::Decimal("123.456".to_string()));
    }

    #[test]
    fn single_float() {
        let got = parse_clixml("<Sg>1.5</Sg>").unwrap();
        if let PsValue::F32(v) = got[0] {
            assert!((v - 1.5).abs() < f32::EPSILON);
        } else {
            panic!("expected F32");
        }
    }

    #[test]
    fn single_float_special() {
        let got = parse_clixml("<Sg>NaN</Sg>").unwrap();
        assert!(matches!(got[0], PsValue::F32(v) if v.is_nan()));
        let got = parse_clixml("<Sg>Infinity</Sg>").unwrap();
        assert!(matches!(got[0], PsValue::F32(v) if v.is_infinite()));
    }

    #[test]
    fn single_float_error() {
        assert!(parse_clixml("<Sg>not-float</Sg>").is_err());
    }

    #[test]
    fn unknown_top_level_element_skipped() {
        let xml = "<FutureTag><nested>data</nested></FutureTag><I32>42</I32>";
        let got = parse_clixml(xml).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], PsValue::I32(42));
    }

    #[test]
    fn unknown_self_closing_element_skipped() {
        let xml = "<UnknownThing/><I32>7</I32>";
        let got = parse_clixml(xml).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], PsValue::I32(7));
    }

    #[test]
    fn empty_s_tag_self_closing() {
        let got = parse_clixml("<S/>").unwrap();
        assert_eq!(got[0], PsValue::String(String::new()));
    }

    #[test]
    fn eof_inside_obj_is_error() {
        assert!(parse_clixml("<Obj RefId=\"0\"><MS>").is_err());
    }

    #[test]
    fn eof_inside_list_is_error() {
        assert!(parse_clixml("<Obj RefId=\"0\"><LST><I32>1</I32>").is_err());
    }

    #[test]
    fn eof_inside_dict_is_error() {
        assert!(parse_clixml("<Obj RefId=\"0\"><DCT><En>").is_err());
    }

    #[test]
    fn eof_inside_text_is_error() {
        assert!(parse_clixml("<S>unclosed").is_err());
    }

    #[test]
    fn dict_entry_with_nil_key_and_value() {
        let xml = r#"<Obj RefId="0"><DCT><En><Nil N="Key"/><Nil N="Value"/></En></DCT></Obj>"#;
        let got = parse_clixml(xml).unwrap();
        if let PsValue::Object(o) = &got[0] {
            if let Some(PsValue::Dict(entries)) = o.get("_value") {
                assert_eq!(entries.len(), 1);
                assert_eq!(entries[0], (PsValue::Null, PsValue::Null));
            } else {
                panic!("expected dict");
            }
        } else {
            panic!("expected object");
        }
    }

    #[test]
    fn ref_inside_member_set() {
        let xml = r#"<Obj RefId="a"><MS><S N="k">v</S></MS></Obj><Obj RefId="b"><MS><Ref N="copy" RefId="a"/></MS></Obj>"#;
        let got = parse_clixml(xml).unwrap();
        if let PsValue::Object(o) = &got[1] {
            assert_eq!(o.get("copy"), Some(&got[0]));
        } else {
            panic!();
        }
    }

    #[test]
    fn ie_que_stk_treated_as_list() {
        for tag in ["IE", "QUE", "STK"] {
            let xml = format!(r#"<Obj RefId="0"><{tag}><I32>1</I32><I32>2</I32></{tag}></Obj>"#);
            let got = parse_clixml(&xml).unwrap();
            if let PsValue::Object(o) = &got[0] {
                assert_eq!(
                    o.get("_value"),
                    Some(&PsValue::List(vec![PsValue::I32(1), PsValue::I32(2)])),
                    "{tag} should be treated as list"
                );
            } else {
                panic!("{tag} should produce object");
            }
        }
    }

    #[test]
    fn tnref_self_closing_in_obj() {
        let xml = r#"
          <Obj RefId="0"><TN RefId="0"><T>Foo</T></TN><MS><S N="k">v</S></MS></Obj>
          <Obj RefId="1"><TNRef RefId="0"/><MS><I32 N="n">7</I32></MS></Obj>
        "#;
        let got = parse_clixml(xml).unwrap();
        if let PsValue::Object(o) = &got[1] {
            assert_eq!(o.type_names, vec!["Foo".to_string()]);
        } else {
            panic!();
        }
    }

    #[test]
    fn nil_inside_list() {
        let xml = r#"<Obj RefId="0"><LST><I32>1</I32><Nil/><I32>3</I32></LST></Obj>"#;
        let got = parse_clixml(xml).unwrap();
        if let PsValue::Object(o) = &got[0] {
            assert_eq!(
                o.get("_value"),
                Some(&PsValue::List(vec![
                    PsValue::I32(1),
                    PsValue::Null,
                    PsValue::I32(3)
                ]))
            );
        } else {
            panic!();
        }
    }

    #[test]
    fn dict_roundtrip() {
        let v = PsValue::Dict(vec![
            (PsValue::String("k1".into()), PsValue::I32(1)),
            (PsValue::String("k2".into()), PsValue::String("v".into())),
        ]);
        let xml = to_clixml(&v);
        let got = parse_clixml(&xml).unwrap();
        if let PsValue::Object(o) = &got[0] {
            if let Some(PsValue::Dict(entries)) = o.get("_value") {
                assert_eq!(entries.len(), 2);
                assert_eq!(entries[0].0, PsValue::String("k1".into()));
                assert_eq!(entries[0].1, PsValue::I32(1));
            } else {
                panic!("dict lost");
            }
        } else {
            panic!();
        }
    }
}
