use std::{
    any::TypeId,
    collections::{BTreeMap, BTreeSet},
    ops::Range,
};

use eframe::egui::{self, TextBuffer, TextEdit, Ui};
use festerm_automation::{Invocation, InvocationBinding, TargetRef};
use festerm_automation_js::snapshot::{ApprovedBundle, Capability, ParameterType, ParameterValue};
use festerm_automation_workflow::review::{
    decode_text_literal, supports_observation_review, Draft, InputValue, ResultRecipient,
    ReviewRevision, MAX_TEXT_LITERAL_BYTES,
};
use festerm_ui_egui::theme;

use crate::automation::{
    NativeCommand, WorkflowReviewProposal, WorkflowReviewRequest, WorkflowReviewView, WorkflowView,
};

use super::{escape_workflow_source, ssh_paragraph, workflow_source_page, AppCommand};

#[derive(Clone)]
struct AsciiInput {
    text: String,
    max_bytes: usize,
    rejected: Option<&'static str>,
}

impl AsciiInput {
    fn new(max_bytes: usize) -> Self {
        Self {
            text: String::new(),
            max_bytes,
            rejected: None,
        }
    }
}

impl TextBuffer for AsciiInput {
    fn is_mutable(&self) -> bool {
        true
    }

    fn as_str(&self) -> &str {
        &self.text
    }

    fn insert_text(&mut self, text: &str, index: egui::text::CharIndex) -> usize {
        if !text.bytes().all(|byte| (b' '..=b'~').contains(&byte)) {
            self.rejected = Some("Only printable ASCII can be entered; use JSON escapes.");
            return 0;
        }
        if self.text.len().saturating_add(text.len()) > self.max_bytes {
            self.rejected = Some("The encoded input is too long; no text was inserted.");
            return 0;
        }
        self.rejected = None;
        <String as TextBuffer>::insert_text(&mut self.text, text, index)
    }

    fn delete_char_range(&mut self, range: Range<egui::text::CharIndex>) {
        <String as TextBuffer>::delete_char_range(&mut self.text, range);
        self.rejected = None;
    }

    fn type_id(&self) -> TypeId {
        TypeId::of::<Self>()
    }
}

#[derive(Clone)]
enum Editor {
    Text(AsciiInput),
    Integer(AsciiInput),
    Boolean(Option<bool>),
    Target(Option<TargetRef>),
}

#[derive(Clone)]
struct DraftState {
    revision: ReviewRevision,
    selected: BTreeSet<TargetRef>,
    parameters: BTreeMap<String, Editor>,
    error: Option<&'static str>,
}

impl DraftState {
    fn new(revision: ReviewRevision, bundle: &ApprovedBundle) -> Self {
        let parameters = bundle
            .manifest()
            .parameters
            .iter()
            .map(|(name, kind)| {
                let editor = match kind {
                    ParameterType::Text { .. } => {
                        Editor::Text(AsciiInput::new(MAX_TEXT_LITERAL_BYTES))
                    }
                    ParameterType::Integer { .. } => Editor::Integer(AsciiInput::new(24)),
                    ParameterType::Boolean {} => Editor::Boolean(None),
                    ParameterType::Target {} => Editor::Target(None),
                };
                (name.clone(), editor)
            })
            .collect();
        Self {
            revision,
            selected: BTreeSet::new(),
            parameters,
            error: None,
        }
    }

    fn draft(&self) -> Result<Draft, &'static str> {
        let mut parameters = BTreeMap::new();
        for (name, editor) in &self.parameters {
            let value = match editor {
                Editor::Text(input) => InputValue::Text(
                    decode_text_literal(&input.text)
                        .map_err(|_| "Enter a complete JSON string for every text parameter.")?,
                ),
                Editor::Integer(input) => InputValue::Integer(
                    input
                        .text
                        .parse()
                        .map_err(|_| "Enter a valid integer for every integer parameter.")?,
                ),
                Editor::Boolean(value) => {
                    InputValue::Boolean(value.ok_or("Choose every boolean parameter explicitly.")?)
                }
                Editor::Target(value) => {
                    InputValue::Target(value.ok_or("Choose every target parameter explicitly.")?)
                }
            };
            parameters.insert(name.clone(), value);
        }
        Ok(Draft {
            parameters,
            targets: self.selected.clone(),
        })
    }
}

#[derive(Clone, Default)]
struct ParameterCursor {
    binding: Option<InvocationBinding>,
    parameter: String,
    page: usize,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(result, "{byte:02x}");
    }
    result
}

fn show_parameter_values(ui: &mut Ui, invocation: &festerm_automation_workflow::FrozenInvocation) {
    ui.collapsing("Review frozen parameter values (not approval)", |ui| {
        let parameters = invocation.parameters();
        if parameters.is_empty() {
            ui.label("No parameters.");
            return;
        }
        let id = ui.id().with("workflow-review-parameter");
        let mut cursor = ui
            .data(|data| data.get_temp::<ParameterCursor>(id))
            .unwrap_or_default();
        if cursor.binding != Some(invocation.binding())
            || !parameters.contains_key(&cursor.parameter)
        {
            cursor = ParameterCursor {
                binding: Some(invocation.binding()),
                parameter: parameters.keys().next().cloned().unwrap_or_default(),
                page: 0,
            };
        }
        egui::ComboBox::from_label("Frozen parameter")
            .selected_text(&cursor.parameter)
            .show_ui(ui, |ui| {
                for name in parameters.keys() {
                    if ui
                        .selectable_label(cursor.parameter == *name, name)
                        .clicked()
                    {
                        cursor.parameter.clone_from(name);
                        cursor.page = 0;
                    }
                }
            });
        match &parameters[&cursor.parameter] {
            ParameterValue::Text(value) => {
                let (page, pages, _) = workflow_source_page(value, cursor.page);
                cursor.page = page;
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(page > 0, egui::Button::new("Previous parameter page"))
                        .clicked()
                    {
                        cursor.page -= 1;
                    }
                    if ui
                        .add_enabled(page + 1 < pages, egui::Button::new("Next parameter page"))
                        .clicked()
                    {
                        cursor.page += 1;
                    }
                });
                let (page, pages, range) = workflow_source_page(value, cursor.page);
                ui.label(format!(
                    "Text page {} of {}; bytes {}-{} of {}",
                    page + 1,
                    pages,
                    if range.is_empty() { 0 } else { range.start + 1 },
                    range.end,
                    value.len()
                ));
                if value.is_empty() {
                    ui.label("(empty text)");
                } else {
                    ui.label(
                        egui::RichText::new(escape_workflow_source(&value[range])).monospace(),
                    );
                }
            }
            ParameterValue::Integer(value) => {
                ui.label(format!("Integer: {value}"));
            }
            ParameterValue::Boolean(value) => {
                ui.label(format!("Boolean: {value}"));
            }
            ParameterValue::Target(alias) => {
                ui.label(format!("Target mapping: {alias}"));
            }
        }
        ui.data_mut(|data| data.insert_temp(id, cursor));
    });
}

fn show_proposal(ui: &mut Ui, proposal: &WorkflowReviewProposal) {
    let invocation = proposal.review().invocation();
    let binding = invocation.binding();
    let Invocation::Workflow { bundle, manifest } = binding.invocation else {
        ui.colored_label(
            theme::STATUS_ERROR,
            "Frozen review identity is invalid; discard it.",
        );
        return;
    };
    ui.separator();
    ui.strong("Frozen review only — not approved and cannot run");
    ui.label(format!("Bundle SHA-256: {}", hex(&bundle.0)));
    ui.label(format!("Manifest SHA-256: {}", hex(&manifest.0)));
    ui.label(format!(
        "Parameters and target mapping SHA-256: {}",
        hex(&binding.parameters.0)
    ));
    ui.label(format!("Workflow API version: {}", binding.api_version));
    match proposal.review().recipient() {
        ResultRecipient::NativeUser => ssh_paragraph(
            ui,
            "Proposed result recipient: the native user who starts this workflow, not an MCP \
             client. No result delivery, grant, expiry, or worker exists in this review.",
        ),
    }
    let scope = invocation.grant_request().scope;
    let capabilities = &invocation.bundle().manifest().capabilities;
    if invocation.targets().is_empty() {
        ui.label("No terminal attachments are requested.");
    }
    for (alias, reference) in invocation.targets() {
        let Some(row) = proposal
            .targets()
            .iter()
            .find(|row| row.reference == *reference)
        else {
            ui.colored_label(
                theme::STATUS_ERROR,
                "Target mapping unavailable; discard this review.",
            );
            return;
        };
        ui.label(format!(
            "{alias}: instance {}, tab {}, attachment {}",
            hex(&reference.application_instance()),
            reference.session(),
            reference.generation()
        ));
        ui.label(&row.label);
        let mut requested = Vec::new();
        if scope.discover.contains(reference) {
            requested.push("discover attachment metadata");
        }
        if scope.observe.contains(reference) {
            for (capability, label) in [
                (Capability::ReadScreen, "read screen"),
                (Capability::ReadScrollback, "read retained history"),
                (Capability::GetSelection, "read selection"),
                (Capability::WaitForChange, "wait for changes"),
            ] {
                if capabilities.contains(&capability) {
                    requested.push(label);
                }
            }
        }
        ui.label(format!("Requested for {alias}: {}", requested.join(", ")));
    }
    ssh_paragraph(
        ui,
        "These are exact shared-broker attachment identifiers, not authority. \
         A future native decision must revalidate this frozen mapping and the privacy fence. \
         Terminal labels are untrusted descriptions, not identity.",
    );
    show_parameter_values(ui, invocation);
}

pub(super) fn clear_draft(ui: &mut Ui) {
    let id = ui.id().with("workflow-review-draft");
    ui.data_mut(|data| data.remove::<DraftState>(id));
}

pub(super) fn show(
    ui: &mut Ui,
    selection: &WorkflowView,
    review: &WorkflowReviewView,
    bundle: &ApprovedBundle,
) -> Option<AppCommand> {
    let Some((revision, _)) = selection.review_token() else {
        clear_draft(ui);
        ui.colored_label(theme::STATUS_ERROR, "Workflow review is unavailable.");
        return None;
    };
    let id = ui.id().with("workflow-review-draft");
    let mut state = ui
        .data(|data| data.get_temp::<DraftState>(id))
        .unwrap_or_else(|| DraftState::new(revision.clone(), bundle));
    if !state.revision.matches(&revision) {
        state = DraftState::new(revision, bundle);
    }

    ui.separator();
    ui.strong("Prepare an observation-only review");
    let mut command = None;
    ssh_paragraph(
        ui,
        "Choose exact live attachments and enter every parameter. Nothing here approves access. \
         Do not enter secrets in parameters. Text uses an ASCII JSON string, such as \"hello\" \
         or \"\\u00e9\"; controls and non-ASCII characters must be escaped.",
    );
    if !supports_observation_review(bundle) {
        ui.colored_label(
            theme::STATUS_ERROR,
            "The manifest requests unsupported control, session creation, or child invocation. \
             This workflow cannot be reviewed as observation-only.",
        );
        ui.data_mut(|data| data.insert_temp(id, state));
        return None;
    }
    if let Some(status) = review.status() {
        ui.colored_label(theme::STATUS_ERROR, status);
    }
    if !review.ready() {
        ssh_paragraph(
            ui,
            "Initialization starts shared privacy monitoring and an in-memory broker only. \
             It does not start MCP, a workflow worker, or grant access. The shared broker \
             can persist until privacy loss or application exit. Native platform \
             qualification remains incomplete.",
        );
    }
    if !review.ready()
        && ui
            .add_enabled(
                !review.starting(),
                egui::Button::new("Initialize workflow review targets"),
            )
            .clicked()
    {
        command = Some(AppCommand::Automation(
            NativeCommand::InitializeWorkflowReview,
        ));
    }
    let live = review.live_targets();
    let mut changed = false;
    let before = state.selected.len();
    state
        .selected
        .retain(|reference| live.iter().any(|row| row.reference == *reference));
    changed |= before != state.selected.len();

    let needs_targets = !bundle.manifest().capabilities.is_empty()
        || bundle
            .manifest()
            .parameters
            .values()
            .any(|kind| matches!(kind, ParameterType::Target {}));
    if needs_targets {
        ui.label("Explicitly select attachments (none is selected by default):");
        if live.is_empty() && review.ready() {
            ui.label("No eligible live attachments are available.");
        }
        egui::ScrollArea::vertical()
            .id_salt("workflow-review-targets")
            .max_height(180.0)
            .show(ui, |ui| {
                for row in &live {
                    ui.push_id(
                        (row.reference.session(), row.reference.generation()),
                        |ui| {
                            let mut selected = state.selected.contains(&row.reference);
                            if ui.checkbox(&mut selected, &row.label).changed() {
                                if selected {
                                    state.selected.insert(row.reference);
                                } else {
                                    state.selected.remove(&row.reference);
                                }
                                changed = true;
                            }
                        },
                    );
                }
            });
    }
    if state.selected.len() > festerm_automation::MAX_SCOPE_TARGETS {
        ui.colored_label(theme::STATUS_ERROR, "Select at most 32 attachments.");
    }

    for (name, kind) in &bundle.manifest().parameters {
        ui.push_id(name, |ui| {
            let Some(editor) = state.parameters.get_mut(name) else {
                return;
            };
            match (kind, editor) {
                (ParameterType::Text { max_bytes }, Editor::Text(input)) => {
                    ui.label(format!("{name}: text, at most {max_bytes} decoded bytes"));
                    changed |= ui
                        .add(TextEdit::singleline(input).hint_text("JSON string, including quotes"))
                        .changed();
                    if let Some(error) = input.rejected {
                        ui.colored_label(theme::STATUS_ERROR, error);
                    }
                }
                (ParameterType::Integer { min, max }, Editor::Integer(input)) => {
                    ui.label(format!("{name}: integer from {min} to {max}"));
                    changed |= ui.add(TextEdit::singleline(input)).changed();
                    if let Some(error) = input.rejected {
                        ui.colored_label(theme::STATUS_ERROR, error);
                    }
                }
                (ParameterType::Boolean {}, Editor::Boolean(value)) => {
                    ui.label(format!("{name}: boolean"));
                    egui::ComboBox::from_id_salt("boolean")
                        .selected_text(match value {
                            Some(true) => "true",
                            Some(false) => "false",
                            None => "Choose true or false",
                        })
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(*value == Some(true), "true").clicked() {
                                *value = Some(true);
                                changed = true;
                            }
                            if ui
                                .selectable_label(*value == Some(false), "false")
                                .clicked()
                            {
                                *value = Some(false);
                                changed = true;
                            }
                        });
                }
                (ParameterType::Target {}, Editor::Target(value)) => {
                    if value.is_some_and(|target| !state.selected.contains(&target)) {
                        *value = None;
                        changed = true;
                    }
                    ui.label(format!("{name}: exact attachment"));
                    let label = value
                        .and_then(|target| live.iter().find(|row| row.reference == target))
                        .map_or("Choose a selected attachment", |row| row.label.as_str());
                    egui::ComboBox::from_id_salt("target")
                        .selected_text(label)
                        .show_ui(ui, |ui| {
                            for row in &live {
                                if state.selected.contains(&row.reference)
                                    && ui
                                        .selectable_label(*value == Some(row.reference), &row.label)
                                        .clicked()
                                {
                                    *value = Some(row.reference);
                                    changed = true;
                                }
                            }
                        });
                }
                _ => {
                    state.error = Some("The parameter schema changed; select the workflow again.");
                }
            }
        });
    }
    if changed {
        state.error = None;
        if let Some(revision) = selection.invalidate_review() {
            state.revision = revision;
        } else {
            state.error = Some("Workflow review is unavailable; select the workflow again.");
        }
    }
    if ui
        .add_enabled(review.ready(), egui::Button::new("Prepare workflow review"))
        .clicked()
    {
        if let Some(revision) = selection.invalidate_review() {
            state.revision = revision;
            match state.draft() {
                Ok(draft) => match WorkflowReviewRequest::new(selection, &state.revision, draft) {
                    Some(request) => {
                        state.error = None;
                        command = Some(AppCommand::Automation(
                            NativeCommand::PrepareWorkflowReview(request),
                        ));
                    }
                    None => state.error = Some("Workflow review is unavailable."),
                },
                Err(error) => state.error = Some(error),
            }
        } else {
            state.error = Some("Workflow review is unavailable; select the workflow again.");
        }
    }
    if let Some(error) = state.error {
        ui.colored_label(theme::STATUS_ERROR, error);
    }
    if let Some(proposal) = review.proposal() {
        show_proposal(ui, proposal);
        if ui.button("Discard workflow review").clicked() {
            command = Some(AppCommand::Automation(NativeCommand::DiscardWorkflowReview));
        }
    }
    ui.data_mut(|data| data.insert_temp(id, state));
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_review_input_rejects_spoofing_and_overflow_without_truncation() {
        let mut input = AsciiInput::new(4);
        assert_eq!(input.insert_text("\"ok\"", egui::text::CharIndex(0)), 4);
        assert_eq!(input.insert_text("\u{202e}", egui::text::CharIndex(4)), 0);
        assert_eq!(input.as_str(), "\"ok\"");
        assert!(input.rejected.is_some());
        assert_eq!(input.insert_text("x", egui::text::CharIndex(4)), 0);
        assert_eq!(input.as_str(), "\"ok\"");
        assert!(input.rejected.is_some());
    }
}
