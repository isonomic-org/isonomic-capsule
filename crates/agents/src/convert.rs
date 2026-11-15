//! The host's types to the SDK's wire types and back, and host errors to
//! wire errors. Ids and events are harness-core's on both sides; only the
//! call arguments and results differ.

use harness_host as host;
use harness_preset::Severity;
use isonomic_proto::agents as wire;
use isonomic_proto::{Bytes, ErrorCode, WireError};

pub(crate) fn error(e: host::HostError) -> WireError {
    use host::HostError as H;
    let (code, message) = match e {
        H::NotFound => (ErrorCode::NotFound, "not found".to_string()),
        // A token the resolver refused; the broker checks tokens first, so
        // this means the approval behind it went away meanwhile.
        H::Unauthorized => (ErrorCode::UnknownToken, "the token is not valid".into()),
        H::ClassNotSupported(class) => (
            ErrorCode::ClassNotSupported,
            format!("sessions of class {class:?} are not run yet"),
        ),
        H::InvalidPreset(diagnostics) => {
            let lines: Vec<String> = diagnostics
                .iter()
                .map(|d| format!("{}: {}", d.path, d.message))
                .collect();
            (ErrorCode::InvalidPreset, lines.join("; "))
        }
        H::NotAtTurnBoundary => (
            ErrorCode::NotAtTurnBoundary,
            "a fork must start at a turn boundary".into(),
        ),
        H::ContinueRefused(why) => (ErrorCode::ContinueRefused, why),
        H::NotAllowedToDecide => (
            ErrorCode::NotAllowedToDecide,
            "this caller may not decide approvals".into(),
        ),
        H::MissingUserAction => (
            ErrorCode::MissingUserAction,
            "a decision needs a person's action".into(),
        ),
        H::AlreadyDecided => (ErrorCode::AlreadyDecided, "already decided".into()),
        H::SessionFailed => (
            ErrorCode::SessionFailed,
            "the session failed and takes no more input".into(),
        ),
        H::InvalidTools(why) => (ErrorCode::InvalidTools, why),
        H::Store(e) => (ErrorCode::Internal, format!("session store: {e}")),
        H::Model(e) => (ErrorCode::ModelUnavailable, e.message),
    };
    WireError::new(code, message)
}

pub(crate) fn summary(s: host::SessionSummary) -> wire::SessionSummary {
    wire::SessionSummary {
        id: s.id,
        app: s.app,
        class: s.class,
        preset: s.preset,
        title: s.title,
        status: s.status,
        archived: s.archived,
        forked_from: s
            .forked_from
            .map(|(session, seq)| wire::ForkedFrom { session, seq }),
        created_at: s.created_at,
        updated_at: s.updated_at,
        last_seq: s.last_seq,
    }
}

pub(crate) fn snapshot(s: host::SessionSnapshot) -> wire::SessionSnapshot {
    wire::SessionSnapshot {
        summary: summary(s.summary),
        turn_open: s.turn_open,
        approvals: s.approvals,
        questions: s.questions,
        awaiting_client: s.awaiting_client,
    }
}

pub(crate) fn filter(l: wire::ListSessions) -> host::SessionFilter {
    host::SessionFilter {
        archived: l.archived,
        preset: l.preset,
        text: l.text,
        cursor: l.cursor,
        limit: if l.limit == 0 { 50 } else { l.limit },
    }
}

pub(crate) fn fork_point(at: wire::ForkPoint) -> host::ForkPoint {
    match at {
        wire::ForkPoint::BeforeTurn(turn) => host::ForkPoint::BeforeTurn(turn),
        wire::ForkPoint::AfterTurn(turn) => host::ForkPoint::AfterTurn(turn),
    }
}

pub(crate) fn decision(d: wire::DecisionInput) -> host::DecisionInput {
    match d {
        wire::DecisionInput::Allow => host::DecisionInput::Allow,
        wire::DecisionInput::Deny => host::DecisionInput::Deny,
        wire::DecisionInput::Text(text) => host::DecisionInput::Text(text),
    }
}

pub(crate) fn approval(a: host::PendingApproval) -> wire::PendingApproval {
    wire::PendingApproval {
        session: a.session,
        app: a.owner,
        request: a.request,
    }
}

pub(crate) fn purge(p: host::PurgeReport) -> wire::PurgeReport {
    wire::PurgeReport {
        sessions: p.sessions,
        events: p.events,
        blobs: p.blobs,
    }
}

fn diagnostic(d: harness_preset::Diagnostic) -> wire::Diagnostic {
    let severity = match d.severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
    };
    wire::Diagnostic {
        severity: severity.into(),
        at: d.path,
        message: d.message,
    }
}

pub(crate) fn preset_summary(p: host::PresetSummary) -> wire::PresetSummary {
    wire::PresetSummary {
        reference: p.reference,
        owner: p.owner,
        name: p.name,
        diagnostics: p.diagnostics.into_iter().map(diagnostic).collect(),
    }
}

pub(crate) fn preset_detail(p: host::StoredPreset) -> wire::PresetDetail {
    wire::PresetDetail {
        reference: p.reference,
        owner: p.owner,
        text: p.text,
        diagnostics: p.diagnostics.into_iter().map(diagnostic).collect(),
    }
}

pub(crate) fn artifact(a: host::ArtifactVersion) -> wire::ArtifactVersion {
    wire::ArtifactVersion {
        meta: a.meta,
        bytes: Bytes(a.bytes),
    }
}
