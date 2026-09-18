//! Executable semantic grammar for dialogs (`docs/dialog-grammar.md`).
//!
//! This model is deliberately separate from [`crate::dialog_layout::DialogSpec`].
//! Layout owns rectangles and stable row budgets; a `DialogFlowSpec` owns the
//! subject-first ordering of a dialog and its current unresolved semantic step.

use std::borrow::Cow;
use std::error::Error;
use std::fmt;

/// The grammar a dialog follows.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlowGrammar {
    /// An established object is shown before an operation is chosen.
    Existing,
    /// Creation begins by choosing the operation or type being created.
    New,
    /// A list chooses an object, then exposes operations for that object.
    Manager,
    /// An established object is inspected before its operations are exposed.
    Inspector,
    /// Read-only content with no subject or operations (Help's exemption).
    Informational,
    /// Nothing is selectable while pending; a returned result becomes the
    /// object shown before its operations.
    AsyncResult,
}

/// The first semantic step that has not yet been resolved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlowPhase {
    ChooseObject,
    ChooseOperation,
    EditParameters,
    Review,
    InspectDetails,
    Read,
    Pending,
    Result,
}

/// Stable identity and user-facing summary for an established object.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObjectSummary {
    id: Cow<'static, str>,
    kind: Cow<'static, str>,
    label: Cow<'static, str>,
}

impl ObjectSummary {
    pub fn new(
        id: impl Into<Cow<'static, str>>,
        kind: impl Into<Cow<'static, str>>,
        label: impl Into<Cow<'static, str>>,
    ) -> Result<Self, DialogFlowError> {
        let summary = Self {
            id: id.into(),
            kind: kind.into(),
            label: label.into(),
        };
        require_text("object id", &summary.id)?;
        require_text("object kind", &summary.kind)?;
        require_text("object label", &summary.label)?;
        Ok(summary)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn kind(&self) -> &str {
        &self.kind
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

/// What the dialog is about. `None` is valid only before a manager selection
/// or while an asynchronous flow has no result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Subject {
    Existing(ObjectSummary),
    New {
        kind: Cow<'static, str>,
        origin: Option<ObjectSummary>,
    },
    None,
}

impl Subject {
    fn description(&self) -> &'static str {
        match self {
            Self::Existing(_) => "existing",
            Self::New { .. } => "new",
            Self::None => "none",
        }
    }
}

/// Stable operation identity. Labels may change without changing this value.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct OperationId(Cow<'static, str>);

impl OperationId {
    pub fn new(value: impl Into<Cow<'static, str>>) -> Result<Self, DialogFlowError> {
        let value = value.into();
        require_text("operation id", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for OperationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A distinct group of controls used by one operation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParameterGroup {
    id: Cow<'static, str>,
    label: Cow<'static, str>,
}

impl ParameterGroup {
    pub fn new(
        id: impl Into<Cow<'static, str>>,
        label: impl Into<Cow<'static, str>>,
    ) -> Result<Self, DialogFlowError> {
        let group = Self {
            id: id.into(),
            label: label.into(),
        };
        require_text("parameter group id", &group.id)?;
        require_text("parameter group label", &group.label)?;
        Ok(group)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

/// One explicit operation and the controls shown only when it is selected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperationSpec {
    id: OperationId,
    label: Cow<'static, str>,
    parameters: Option<ParameterGroup>,
    submit_label: Cow<'static, str>,
    destructive: bool,
}

impl OperationSpec {
    pub fn new(
        id: impl Into<Cow<'static, str>>,
        label: impl Into<Cow<'static, str>>,
        submit_label: impl Into<Cow<'static, str>>,
    ) -> Result<Self, DialogFlowError> {
        let operation = Self {
            id: OperationId::new(id)?,
            label: label.into(),
            parameters: None,
            submit_label: submit_label.into(),
            destructive: false,
        };
        require_text("operation label", &operation.label)?;
        require_text("submit label", &operation.submit_label)?;
        Ok(operation)
    }

    #[must_use]
    pub fn with_parameters(mut self, parameters: ParameterGroup) -> Self {
        self.parameters = Some(parameters);
        self
    }

    #[must_use]
    pub fn destructive(mut self, destructive: bool) -> Self {
        self.destructive = destructive;
        self
    }

    pub fn id(&self) -> &OperationId {
        &self.id
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn parameters(&self) -> Option<&ParameterGroup> {
        self.parameters.as_ref()
    }

    pub fn submit_label(&self) -> &str {
        &self.submit_label
    }

    pub fn is_destructive(&self) -> bool {
        self.destructive
    }
}

/// A validated dialog flow. Fields stay private so a component cannot publish
/// an impossible phase/subject/operation combination at the shared seam.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DialogFlowSpec {
    grammar: FlowGrammar,
    object_kind: Cow<'static, str>,
    subject: Subject,
    operations: Vec<OperationSpec>,
    selected_operation: Option<OperationId>,
    phase: FlowPhase,
}

impl DialogFlowSpec {
    pub fn existing(
        subject: ObjectSummary,
        operations: impl IntoIterator<Item = OperationSpec>,
    ) -> Result<Self, DialogFlowError> {
        let object_kind = subject.kind.clone();
        Self::build(
            FlowGrammar::Existing,
            object_kind,
            Subject::Existing(subject),
            operations,
        )
    }

    pub fn new_object(
        kind: impl Into<Cow<'static, str>>,
        origin: Option<ObjectSummary>,
        operations: impl IntoIterator<Item = OperationSpec>,
    ) -> Result<Self, DialogFlowError> {
        let kind = kind.into();
        require_text("object kind", &kind)?;
        Self::build(
            FlowGrammar::New,
            kind.clone(),
            Subject::New { kind, origin },
            operations,
        )
    }

    pub fn manager(
        object_kind: impl Into<Cow<'static, str>>,
        operations: impl IntoIterator<Item = OperationSpec>,
    ) -> Result<Self, DialogFlowError> {
        Self::build(
            FlowGrammar::Manager,
            object_kind.into(),
            Subject::None,
            operations,
        )
    }

    pub fn inspector(
        subject: ObjectSummary,
        operations: impl IntoIterator<Item = OperationSpec>,
    ) -> Result<Self, DialogFlowError> {
        let object_kind = subject.kind.clone();
        Self::build(
            FlowGrammar::Inspector,
            object_kind,
            Subject::Existing(subject),
            operations,
        )
    }

    pub fn async_result(
        result_kind: impl Into<Cow<'static, str>>,
        operations: impl IntoIterator<Item = OperationSpec>,
    ) -> Result<Self, DialogFlowError> {
        Self::build(
            FlowGrammar::AsyncResult,
            result_kind.into(),
            Subject::None,
            operations,
        )
    }

    pub fn informational() -> Self {
        Self {
            grammar: FlowGrammar::Informational,
            object_kind: Cow::Borrowed("informational"),
            subject: Subject::None,
            operations: Vec::new(),
            selected_operation: None,
            phase: FlowPhase::Read,
        }
    }

    fn build(
        grammar: FlowGrammar,
        object_kind: Cow<'static, str>,
        subject: Subject,
        operations: impl IntoIterator<Item = OperationSpec>,
    ) -> Result<Self, DialogFlowError> {
        require_text("object kind", &object_kind)?;
        let flow = Self {
            grammar,
            object_kind,
            subject,
            operations: operations.into_iter().collect(),
            selected_operation: None,
            phase: initial_phase(grammar),
        };
        flow.validate()?;
        Ok(flow)
    }

    pub fn grammar(&self) -> FlowGrammar {
        self.grammar
    }

    pub fn subject(&self) -> &Subject {
        &self.subject
    }

    pub fn operations(&self) -> &[OperationSpec] {
        &self.operations
    }

    pub fn selected_operation(&self) -> Option<&OperationId> {
        self.selected_operation.as_ref()
    }

    pub fn phase(&self) -> FlowPhase {
        self.phase
    }

    /// The first unresolved phase on a fresh instance of this grammar.
    pub fn initial_unresolved_phase(&self) -> FlowPhase {
        initial_phase(self.grammar)
    }

    /// Semantic phase order after the subject carried by the spec. Empty
    /// parameter bands may be skipped; managers hand parameterized operations
    /// to a child/replacement flow after `ChooseOperation`.
    pub fn required_order(&self) -> &'static [FlowPhase] {
        match self.grammar {
            FlowGrammar::Existing | FlowGrammar::New => &[
                FlowPhase::ChooseOperation,
                FlowPhase::EditParameters,
                FlowPhase::Review,
            ],
            FlowGrammar::Manager => &[FlowPhase::ChooseObject, FlowPhase::ChooseOperation],
            FlowGrammar::Inspector => &[FlowPhase::InspectDetails, FlowPhase::ChooseOperation],
            FlowGrammar::Informational => &[FlowPhase::Read],
            FlowGrammar::AsyncResult => &[
                FlowPhase::Pending,
                FlowPhase::Result,
                FlowPhase::ChooseOperation,
                FlowPhase::EditParameters,
                FlowPhase::Review,
            ],
        }
    }

    /// Select or change the object in a manager. Changing rows clears the
    /// prior operation and returns to operation choice; it never silently
    /// enters Edit for the new object.
    pub fn select_object(&mut self, subject: ObjectSummary) -> Result<(), DialogFlowError> {
        self.validate()?;
        if self.grammar != FlowGrammar::Manager {
            return Err(self.invalid_transition("select object"));
        }
        if subject.kind() != self.object_kind {
            return Err(DialogFlowError::WrongObjectKind {
                expected: self.object_kind.to_string(),
                actual: subject.kind().to_owned(),
            });
        }
        self.subject = Subject::Existing(subject);
        self.selected_operation = None;
        self.phase = FlowPhase::ChooseOperation;
        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    /// Select an operation only from the operation-choice phase. Operations
    /// without controls skip the empty parameter band and become review-ready.
    pub fn select_operation(&mut self, id: &OperationId) -> Result<(), DialogFlowError> {
        self.validate()?;
        if self.phase != FlowPhase::ChooseOperation {
            return Err(self.invalid_transition("select operation"));
        }
        let operation = self
            .operations
            .iter()
            .find(|operation| operation.id() == id)
            .ok_or_else(|| DialogFlowError::UnknownOperation(id.to_string()))?;
        let next = if operation.parameters().is_some() {
            FlowPhase::EditParameters
        } else {
            FlowPhase::Review
        };
        self.selected_operation = Some(id.clone());
        self.phase = next;
        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    /// Mark the selected operation's parameters ready for review.
    pub fn parameters_ready(&mut self) -> Result<(), DialogFlowError> {
        self.validate()?;
        if self.phase != FlowPhase::EditParameters {
            return Err(self.invalid_transition("finish parameters"));
        }
        self.phase = FlowPhase::Review;
        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    /// Return to operation choice without changing the subject.
    pub fn choose_another_operation(&mut self) -> Result<(), DialogFlowError> {
        self.validate()?;
        if !matches!(self.phase, FlowPhase::EditParameters | FlowPhase::Review) {
            return Err(self.invalid_transition("choose another operation"));
        }
        self.selected_operation = None;
        self.phase = FlowPhase::ChooseOperation;
        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    /// Publish an asynchronous result as the established subject. Result
    /// arrival replaces Pending; request controls cannot remain active above it.
    pub fn resolve_result(&mut self, result: ObjectSummary) -> Result<(), DialogFlowError> {
        self.validate()?;
        if self.grammar != FlowGrammar::AsyncResult || self.phase != FlowPhase::Pending {
            return Err(self.invalid_transition("resolve result"));
        }
        if result.kind() != self.object_kind {
            return Err(DialogFlowError::WrongObjectKind {
                expected: self.object_kind.to_string(),
                actual: result.kind().to_owned(),
            });
        }
        self.subject = Subject::Existing(result);
        self.phase = FlowPhase::Result;
        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    /// Advance from the result/details band to its operations.
    pub fn show_operations(&mut self) -> Result<(), DialogFlowError> {
        self.validate()?;
        let expected = match self.grammar {
            FlowGrammar::AsyncResult => FlowPhase::Result,
            FlowGrammar::Inspector => FlowPhase::InspectDetails,
            _ => return Err(self.invalid_transition("show operations")),
        };
        if self.phase != expected {
            return Err(self.invalid_transition("show operations"));
        }
        self.phase = FlowPhase::ChooseOperation;
        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    /// Re-check the complete state at the component boundary.
    pub fn validate(&self) -> Result<(), DialogFlowError> {
        if self.operations.is_empty() && self.grammar != FlowGrammar::Informational {
            return Err(DialogFlowError::NoOperations);
        }
        if self.grammar == FlowGrammar::Informational && !self.operations.is_empty() {
            return Err(DialogFlowError::OperationNotAllowed(self.phase));
        }
        for (index, operation) in self.operations.iter().enumerate() {
            if self.operations[..index]
                .iter()
                .any(|earlier| earlier.id() == operation.id())
            {
                return Err(DialogFlowError::DuplicateOperation(
                    operation.id().to_string(),
                ));
            }
        }

        let subject_ok = match self.grammar {
            FlowGrammar::Existing | FlowGrammar::Inspector | FlowGrammar::New => true,
            FlowGrammar::Manager => {
                matches!(
                    (&self.subject, self.phase),
                    (Subject::None, FlowPhase::ChooseObject)
                        | (Subject::Existing(_), FlowPhase::ChooseOperation)
                        | (Subject::Existing(_), FlowPhase::EditParameters)
                        | (Subject::Existing(_), FlowPhase::Review)
                )
            }
            FlowGrammar::AsyncResult => {
                matches!(
                    (&self.subject, self.phase),
                    (Subject::None, FlowPhase::Pending)
                        | (Subject::Existing(_), FlowPhase::Result)
                        | (Subject::Existing(_), FlowPhase::ChooseOperation)
                        | (Subject::Existing(_), FlowPhase::EditParameters)
                        | (Subject::Existing(_), FlowPhase::Review)
                )
            }
            FlowGrammar::Informational => {
                self.subject == Subject::None && self.phase == FlowPhase::Read
            }
        };
        if !subject_ok || !self.fixed_subject_matches_grammar() {
            return Err(DialogFlowError::SubjectMismatch {
                grammar: self.grammar,
                subject: self.subject.description(),
            });
        }

        let selected = self
            .selected_operation
            .as_ref()
            .map(|id| {
                self.operations
                    .iter()
                    .find(|operation| operation.id() == id)
                    .ok_or_else(|| DialogFlowError::UnknownOperation(id.to_string()))
            })
            .transpose()?;
        match self.phase {
            FlowPhase::EditParameters => {
                if selected.and_then(OperationSpec::parameters).is_none() {
                    return Err(DialogFlowError::ParametersUnavailable);
                }
            }
            FlowPhase::Review => {
                if selected.is_none() {
                    return Err(DialogFlowError::OperationRequired(self.phase));
                }
            }
            FlowPhase::ChooseObject
            | FlowPhase::ChooseOperation
            | FlowPhase::InspectDetails
            | FlowPhase::Read
            | FlowPhase::Pending
            | FlowPhase::Result => {
                if selected.is_some() {
                    return Err(DialogFlowError::OperationNotAllowed(self.phase));
                }
            }
        }
        Ok(())
    }

    fn fixed_subject_matches_grammar(&self) -> bool {
        match (&self.grammar, &self.subject, self.phase) {
            (FlowGrammar::Existing, Subject::Existing(subject), phase) => {
                subject.kind() == self.object_kind
                    && matches!(
                        phase,
                        FlowPhase::ChooseOperation | FlowPhase::EditParameters | FlowPhase::Review
                    )
            }
            (FlowGrammar::New, Subject::New { kind, .. }, phase) => {
                kind == &self.object_kind
                    && matches!(
                        phase,
                        FlowPhase::ChooseOperation | FlowPhase::EditParameters | FlowPhase::Review
                    )
            }
            (FlowGrammar::Inspector, Subject::Existing(subject), phase) => {
                subject.kind() == self.object_kind
                    && matches!(
                        phase,
                        FlowPhase::InspectDetails
                            | FlowPhase::ChooseOperation
                            | FlowPhase::EditParameters
                            | FlowPhase::Review
                    )
            }
            (FlowGrammar::Informational, Subject::None, FlowPhase::Read) => true,
            (FlowGrammar::Manager, Subject::Existing(subject), _)
            | (FlowGrammar::AsyncResult, Subject::Existing(subject), _) => {
                subject.kind() == self.object_kind
            }
            (FlowGrammar::Manager, Subject::None, FlowPhase::ChooseObject)
            | (FlowGrammar::AsyncResult, Subject::None, FlowPhase::Pending) => true,
            _ => false,
        }
    }

    fn invalid_transition(&self, action: &'static str) -> DialogFlowError {
        DialogFlowError::InvalidTransition {
            grammar: self.grammar,
            phase: self.phase,
            action,
        }
    }
}

fn initial_phase(grammar: FlowGrammar) -> FlowPhase {
    match grammar {
        FlowGrammar::Existing | FlowGrammar::New => FlowPhase::ChooseOperation,
        FlowGrammar::Manager => FlowPhase::ChooseObject,
        FlowGrammar::Inspector => FlowPhase::InspectDetails,
        FlowGrammar::Informational => FlowPhase::Read,
        FlowGrammar::AsyncResult => FlowPhase::Pending,
    }
}

fn require_text(field: &'static str, value: &str) -> Result<(), DialogFlowError> {
    if value.trim().is_empty() {
        Err(DialogFlowError::EmptyField(field))
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DialogFlowError {
    EmptyField(&'static str),
    NoOperations,
    DuplicateOperation(String),
    SubjectMismatch {
        grammar: FlowGrammar,
        subject: &'static str,
    },
    WrongObjectKind {
        expected: String,
        actual: String,
    },
    UnknownOperation(String),
    OperationRequired(FlowPhase),
    OperationNotAllowed(FlowPhase),
    ParametersUnavailable,
    InvalidTransition {
        grammar: FlowGrammar,
        phase: FlowPhase,
        action: &'static str,
    },
}

impl fmt::Display for DialogFlowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyField(field) => write!(formatter, "{field} must not be empty"),
            Self::NoOperations => formatter.write_str("a dialog flow needs at least one operation"),
            Self::DuplicateOperation(id) => write!(formatter, "duplicate operation id `{id}`"),
            Self::SubjectMismatch { grammar, subject } => {
                write!(
                    formatter,
                    "{grammar:?} flow cannot use {subject} subject in this phase"
                )
            }
            Self::WrongObjectKind { expected, actual } => {
                write!(
                    formatter,
                    "expected object kind `{expected}`, got `{actual}`"
                )
            }
            Self::UnknownOperation(id) => write!(formatter, "unknown operation `{id}`"),
            Self::OperationRequired(phase) => {
                write!(formatter, "phase {phase:?} requires a selected operation")
            }
            Self::OperationNotAllowed(phase) => {
                write!(
                    formatter,
                    "phase {phase:?} cannot retain a selected operation"
                )
            }
            Self::ParametersUnavailable => {
                formatter.write_str("parameter phase requires an operation with parameters")
            }
            Self::InvalidTransition {
                grammar,
                phase,
                action,
            } => write!(
                formatter,
                "cannot {action} from {phase:?} in a {grammar:?} flow"
            ),
        }
    }
}

impl Error for DialogFlowError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{Component, Ctx, Event, Outcome, RenderCtx, Surface};
    use ratatui::{Frame, layout::Rect};

    fn object(id: &'static str, kind: &'static str, label: &'static str) -> ObjectSummary {
        ObjectSummary::new(id, kind, label).unwrap()
    }

    fn operation(id: &'static str, label: &'static str, parameters: bool) -> OperationSpec {
        let operation = OperationSpec::new(id, label, "Apply").unwrap();
        if parameters {
            operation.with_parameters(ParameterGroup::new("form", "Parameters").unwrap())
        } else {
            operation
        }
    }

    #[test]
    fn existing_flow_keeps_subject_before_operation_parameters_and_review() {
        let subject = object("view-1", "view", "API errors");
        let mut flow =
            DialogFlowSpec::existing(subject.clone(), [operation("filter", "Filter", true)])
                .unwrap();

        assert_eq!(flow.subject(), &Subject::Existing(subject));
        assert_eq!(flow.phase(), FlowPhase::ChooseOperation);
        assert_eq!(
            flow.required_order(),
            &[
                FlowPhase::ChooseOperation,
                FlowPhase::EditParameters,
                FlowPhase::Review,
            ]
        );

        let filter = OperationId::new("filter").unwrap();
        flow.select_operation(&filter).unwrap();
        assert_eq!(flow.phase(), FlowPhase::EditParameters);
        flow.parameters_ready().unwrap();
        assert_eq!(flow.phase(), FlowPhase::Review);
        assert_eq!(flow.selected_operation(), Some(&filter));
    }

    #[test]
    fn new_flow_starts_with_type_and_skips_an_empty_parameter_band() {
        let origin = object("view-1", "view", "API errors");
        let mut flow = DialogFlowSpec::new_object(
            "view",
            Some(origin.clone()),
            [operation("blank", "New blank view", false)],
        )
        .unwrap();

        assert_eq!(
            flow.subject(),
            &Subject::New {
                kind: Cow::Borrowed("view"),
                origin: Some(origin),
            }
        );
        assert_eq!(flow.phase(), FlowPhase::ChooseOperation);
        flow.select_operation(&OperationId::new("blank").unwrap())
            .unwrap();
        assert_eq!(flow.phase(), FlowPhase::Review);
    }

    #[test]
    fn manager_requires_an_object_and_row_changes_do_not_silently_edit() {
        let mut flow = DialogFlowSpec::manager(
            "source",
            [
                operation("restart", "Restart", false),
                operation("remove", "Remove", true).destructive(true),
            ],
        )
        .unwrap();

        assert_eq!(flow.phase(), FlowPhase::ChooseObject);
        let before = flow.clone();
        assert!(
            flow.select_operation(&OperationId::new("restart").unwrap())
                .is_err()
        );
        assert_eq!(flow, before);

        flow.select_object(object("source-1", "source", "payments"))
            .unwrap();
        flow.select_operation(&OperationId::new("remove").unwrap())
            .unwrap();
        assert_eq!(flow.phase(), FlowPhase::EditParameters);

        flow.select_object(object("source-2", "source", "orders"))
            .unwrap();
        assert_eq!(flow.phase(), FlowPhase::ChooseOperation);
        assert_eq!(flow.selected_operation(), None);
    }

    #[test]
    fn async_flow_replaces_pending_with_result_before_exposing_operations() {
        let mut flow =
            DialogFlowSpec::async_result("proposal", [operation("apply", "Apply", true)]).unwrap();
        assert_eq!(flow.phase(), FlowPhase::Pending);
        assert_eq!(flow.subject(), &Subject::None);

        flow.resolve_result(object("proposal-1", "proposal", "Filter proposal"))
            .unwrap();
        assert_eq!(flow.phase(), FlowPhase::Result);
        assert!(matches!(flow.subject(), Subject::Existing(_)));
        flow.show_operations().unwrap();
        flow.select_operation(&OperationId::new("apply").unwrap())
            .unwrap();
        assert_eq!(flow.phase(), FlowPhase::EditParameters);
    }

    #[test]
    fn every_grammar_reports_its_initial_unresolved_phase() {
        let operations = || [operation("open", "Open", false)];
        let cases = [
            (
                DialogFlowSpec::existing(object("1", "view", "View"), operations()).unwrap(),
                FlowPhase::ChooseOperation,
            ),
            (
                DialogFlowSpec::new_object("view", None, operations()).unwrap(),
                FlowPhase::ChooseOperation,
            ),
            (
                DialogFlowSpec::manager("view", operations()).unwrap(),
                FlowPhase::ChooseObject,
            ),
            (
                DialogFlowSpec::inspector(object("1", "record", "Record 1"), operations()).unwrap(),
                FlowPhase::InspectDetails,
            ),
            (
                DialogFlowSpec::async_result("proposal", operations()).unwrap(),
                FlowPhase::Pending,
            ),
            (DialogFlowSpec::informational(), FlowPhase::Read),
        ];

        for (flow, expected) in cases {
            assert_eq!(flow.initial_unresolved_phase(), expected);
            assert_eq!(flow.phase(), expected);
        }
    }

    #[test]
    fn invalid_definitions_and_transitions_fail_closed() {
        let duplicate = operation("edit", "Edit", true);
        assert!(matches!(
            DialogFlowSpec::existing(
                object("1", "view", "View"),
                [duplicate.clone(), duplicate]
            ),
            Err(DialogFlowError::DuplicateOperation(id)) if id == "edit"
        ));

        let mut flow =
            DialogFlowSpec::async_result("proposal", [operation("apply", "Apply", true)]).unwrap();
        let pending = flow.clone();
        assert!(flow.parameters_ready().is_err());
        assert_eq!(flow, pending);
        assert!(
            flow.resolve_result(object("wrong", "view", "Wrong kind"))
                .is_err()
        );
        assert_eq!(flow, pending);

        flow.resolve_result(object("proposal", "proposal", "Proposal"))
            .unwrap();
        let result = flow.clone();
        assert!(
            flow.select_operation(&OperationId::new("missing").unwrap())
                .is_err()
        );
        assert_eq!(flow, result);
        assert!(
            flow.resolve_result(object("again", "proposal", "Another"))
                .is_err()
        );
        assert_eq!(flow, result);
    }

    struct FlowAwareComponent {
        flow: DialogFlowSpec,
    }

    impl Component for FlowAwareComponent {
        type Hit = ();
        type Open = ();

        fn open(&mut self, _params: Self::Open, _ctx: &mut Ctx<'_>) {}

        fn flow_spec(&self) -> Option<&DialogFlowSpec> {
            Some(&self.flow)
        }

        fn handle(&mut self, _event: Event<Self::Hit>, _ctx: &mut Ctx<'_>) -> Outcome {
            Outcome::Ignored
        }

        fn render(&mut self, _frame: &mut Frame<'_>, _area: Rect, _ctx: &RenderCtx<'_>) -> Surface {
            Surface::default()
        }

        fn surface(&self) -> Surface {
            Surface::default()
        }

        fn hit(&self, _point: (u16, u16)) -> Option<Self::Hit> {
            None
        }
    }

    #[test]
    fn component_seam_exposes_only_a_validated_flow() {
        let component = FlowAwareComponent {
            flow: DialogFlowSpec::existing(
                object("view-1", "view", "API errors"),
                [operation("filter", "Filter", true)],
            )
            .unwrap(),
        };

        let flow = component.flow_spec().expect("component opted in");
        assert_eq!(flow.phase(), FlowPhase::ChooseOperation);
        assert_eq!(flow.validate(), Ok(()));
    }
}
