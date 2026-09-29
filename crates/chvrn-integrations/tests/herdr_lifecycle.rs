use chvrn_integrations::herdr::{
    AgentSessionIdentity, BridgeEffect, HerdrBridge, HunkRange, LifecycleReliability, ReviewReport,
    ReviewedFile,
};
use serde_json::json;

fn envelope(status: &str, pane: &str, revision: u64, seq: u64) -> String {
    envelope_on_terminal(status, pane, revision, seq, "term_65c87de10d6b710")
}

fn envelope_on_terminal(
    status: &str,
    pane: &str,
    revision: u64,
    seq: u64,
    terminal_id: &str,
) -> String {
    envelope_with_agent(status, pane, revision, seq, terminal_id, "omp")
}

fn envelope_with_agent(
    status: &str,
    pane: &str,
    revision: u64,
    seq: u64,
    terminal_id: &str,
    agent: &str,
) -> String {
    json!({
        "id": "cli:agent:get",
        "result": {
            "agent": {
                "agent": agent,
                "agent_status": status,
                "cwd": "/tmp/project",
                "focused": true,
                "foreground_cwd": "/tmp/project",
                "pane_id": pane,
                "revision": revision,
                "state_change_seq": seq,
                "tab_id": "w9:t1",
                "terminal_id": terminal_id,
                "terminal_title": "Agent session",
                "terminal_title_stripped": "Agent session",
                "workspace_id": "w9"
            },
            "type": "agent_info"
        }
    })
    .to_string()
}

fn bridge() -> HerdrBridge {
    let mut bridge = HerdrBridge::new("w9:p5");
    assert!(
        bridge
            .observe_agent_session(AgentSessionIdentity::Verified("agent-run-one".into()))
            .is_empty()
    );
    bridge
}

fn report() -> ReviewReport {
    ReviewReport {
        id: "review-42".into(),
        pane_id: "w9:p5".into(),
        terminal_id: "term_65c87de10d6b710".into(),
        agent_session_id: "agent-run-one".into(),
        snapshot_id: "snapshot-17".into(),
        files: vec![ReviewedFile {
            path: "src/main.rs".into(),
            snapshot_id: "snapshot-17:0".into(),
            accepted: vec![HunkRange { start: 2, end: 4 }],
            rejected: vec![HunkRange { start: 8, end: 10 }],
        }],
        comment: "Keep the first change, revise the second.".into(),
    }
}

#[test]
fn verified_working_to_blocked_offers_one_review_without_refocusing_on_repeated_observations() {
    let mut bridge = bridge();
    assert!(
        bridge
            .observe(
                &envelope("working", "w9:p5", 31, 5),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );

    let transition = bridge
        .observe(
            &envelope("blocked", "w9:p5", 31, 6),
            LifecycleReliability::Verified,
        )
        .unwrap();
    assert!(
        matches!(transition.as_slice(), [BridgeEffect::OfferReview { pane_id, revision }] if pane_id == "w9:p5" && *revision == 31)
    );
    assert!(
        bridge
            .observe(
                &envelope("blocked", "w9:p5", 31, 6),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn verified_working_to_done_offers_review_but_initial_idle_does_not() {
    let mut bridge = bridge();
    assert!(
        bridge
            .observe(
                &envelope("idle", "w9:p5", 31, 5),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
    assert!(
        bridge
            .observe(
                &envelope("working", "w9:p5", 31, 6),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
    let transition = bridge
        .observe(
            &envelope("done", "w9:p5", 31, 7),
            LifecycleReliability::Verified,
        )
        .unwrap();
    assert!(
        matches!(transition.as_slice(), [BridgeEffect::OfferReview { pane_id, revision }] if pane_id == "w9:p5" && *revision == 31)
    );
}

#[test]
fn completed_official_turn_dispatches_submitted_feedback_once_without_waiting_for_idle() {
    let mut bridge = bridge();
    assert!(
        bridge
            .observe(
                &envelope("working", "w9:p5", 327, 28),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
    let review = bridge
        .observe(
            &envelope("done", "w9:p5", 753, 29),
            LifecycleReliability::Verified,
        )
        .unwrap();
    assert!(
        matches!(review.as_slice(), [BridgeEffect::OfferReview { pane_id, .. }] if pane_id == "w9:p5")
    );

    bridge.submit(report()).unwrap();
    let send = bridge
        .observe(
            &envelope("done", "w9:p5", 1102, 29),
            LifecycleReliability::Verified,
        )
        .unwrap();
    assert!(
        matches!(send.as_slice(), [BridgeEffect::SendFeedback { report }] if report.id == "review-42")
    );
    assert!(
        bridge
            .observe(
                &envelope("done", "w9:p5", 1103, 29),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
    bridge.feedback_delivered("review-42").unwrap();
    assert!(
        bridge
            .observe(
                &envelope("done", "w9:p5", 1104, 29),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn unverified_done_cannot_dispatch_feedback_until_authority_is_restored() {
    let mut bridge = bridge();
    assert!(
        bridge
            .observe(
                &envelope("done", "w9:p5", 31, 5),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
    bridge.submit(report()).unwrap();
    assert!(
        bridge
            .observe(
                &envelope("done", "w9:p5", 32, 5),
                LifecycleReliability::Unverified
            )
            .unwrap()
            .is_empty()
    );
    assert!(
        matches!(bridge.observe(&envelope("done", "w9:p5", 33, 5), LifecycleReliability::Verified).unwrap().as_slice(),
        [BridgeEffect::SendFeedback { report }] if report.id == "review-42")
    );
}

#[test]
fn authoritative_omp_working_to_idle_offers_one_review_when_the_hook_reports_completion() {
    let mut bridge = bridge();
    assert!(
        bridge
            .observe(
                &envelope("working", "w9:p5", 31, 5),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
    let effects = bridge
        .observe(
            &envelope("idle", "w9:p5", 32, 6),
            LifecycleReliability::Verified,
        )
        .unwrap();
    assert!(
        matches!(effects.as_slice(), [BridgeEffect::OfferReview { pane_id, .. }] if pane_id == "w9:p5")
    );
    assert!(
        bridge
            .observe(
                &envelope("idle", "w9:p5", 33, 6),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn explicitly_submitted_review_at_initial_verified_idle_sends_once_without_an_agent_turn() {
    let mut bridge = bridge();
    assert!(
        bridge
            .observe(
                &envelope("idle", "w9:p5", 31, 5),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        bridge.explicit_gate(),
        BridgeEffect::OfferReview { .. }
    ));
    bridge.submit(report()).unwrap();
    let send = bridge
        .observe(
            &envelope("idle", "w9:p5", 31, 5),
            LifecycleReliability::Verified,
        )
        .unwrap();
    assert!(
        matches!(send.as_slice(), [BridgeEffect::SendFeedback { report }] if report.id == "review-42")
    );
    assert!(
        bridge
            .observe(
                &envelope("idle", "w9:p5", 31, 5),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn unreliable_idle_during_active_work_never_offers_review_or_dispatches_feedback() {
    let mut bridge = bridge();
    assert!(
        bridge
            .observe(
                &envelope("idle", "w9:p5", 8165, 7),
                LifecycleReliability::Unverified
            )
            .unwrap()
            .is_empty()
    );
    bridge.submit(report()).unwrap();
    for _ in 0..3 {
        assert!(
            bridge
                .observe(
                    &envelope("idle", "w9:p5", 8165, 7),
                    LifecycleReliability::Unverified
                )
                .unwrap()
                .is_empty()
        );
    }
    assert!(matches!(
        bridge.explicit_gate(),
        BridgeEffect::OfferReview { .. }
    ));
    assert!(
        bridge
            .observe(
                &envelope("blocked", "w9:p5", 8166, 8),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
    let send = bridge
        .observe(
            &envelope("idle", "w9:p5", 8167, 9),
            LifecycleReliability::Verified,
        )
        .unwrap();
    assert!(
        matches!(send.as_slice(), [BridgeEffect::SendFeedback { report }] if report.id == "review-42")
    );
}

#[test]
fn submitted_review_waits_through_blocked_permission_state_then_dispatches_once_at_verified_idle() {
    let mut bridge = bridge();
    bridge
        .observe(
            &envelope("blocked", "w9:p5", 31, 6),
            LifecycleReliability::Verified,
        )
        .unwrap();
    bridge.submit(report()).unwrap();
    assert!(
        bridge
            .observe(
                &envelope("blocked", "w9:p5", 31, 6),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
    let effects = bridge
        .observe(
            &envelope("idle", "w9:p5", 31, 7),
            LifecycleReliability::Verified,
        )
        .unwrap();
    assert!(
        matches!(effects.as_slice(), [BridgeEffect::SendFeedback { report }] if report.pane_id == "w9:p5" && report.snapshot_id == "snapshot-17" && report.files[0].accepted[0].start == 2 && report.files[0].rejected[0].end == 10 && report.comment == "Keep the first change, revise the second.")
    );
    assert!(
        bridge
            .observe(
                &envelope("idle", "w9:p5", 31, 7),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
    bridge.feedback_delivered("review-42").unwrap();
    assert!(
        bridge
            .observe(
                &envelope("idle", "w9:p5", 31, 8),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn failed_prompt_can_be_retried_without_claiming_it_was_delivered() {
    let mut bridge = bridge();
    bridge
        .observe(
            &envelope("blocked", "w9:p5", 31, 6),
            LifecycleReliability::Verified,
        )
        .unwrap();
    bridge.submit(report()).unwrap();
    assert!(matches!(
        bridge
            .observe(
                &envelope("idle", "w9:p5", 31, 7),
                LifecycleReliability::Verified
            )
            .unwrap()
            .as_slice(),
        [BridgeEffect::SendFeedback { .. }]
    ));
    bridge.feedback_failed("review-42").unwrap();
    assert!(matches!(
        bridge
            .observe(
                &envelope("idle", "w9:p5", 31, 8),
                LifecycleReliability::Verified
            )
            .unwrap()
            .as_slice(),
        [BridgeEffect::SendFeedback { .. }]
    ));
}

#[test]
fn malformed_unknown_and_error_envelopes_cannot_approve_or_lose_submitted_feedback() {
    let mut bridge = bridge();
    bridge
        .observe(
            &envelope("blocked", "w9:p5", 31, 6),
            LifecycleReliability::Verified,
        )
        .unwrap();
    bridge.submit(report()).unwrap();
    assert!(
        bridge
            .observe("{invalid", LifecycleReliability::Verified)
            .is_err()
    );
    assert!(
        bridge
            .observe(
                r#"{"id":"cli:agent:get","error":{"message":"unavailable"}}"#,
                LifecycleReliability::Verified
            )
            .is_err()
    );
    assert!(
        bridge
            .observe(
                &envelope("unknown_new_status", "w9:p5", 31, 7),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
    assert!(
        matches!(bridge.observe(&envelope("idle", "w9:p5", 31, 8), LifecycleReliability::Verified).unwrap().as_slice(), [BridgeEffect::SendFeedback { report }] if report.id == "review-42")
    );
}

#[test]
fn quit_does_not_submit_or_approve_and_preserves_already_submitted_feedback() {
    let mut bridge = bridge();
    bridge
        .observe(
            &envelope("blocked", "w9:p5", 31, 6),
            LifecycleReliability::Verified,
        )
        .unwrap();
    bridge.quit();
    assert!(
        bridge
            .observe(
                &envelope("idle", "w9:p5", 31, 7),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
    bridge.submit(report()).unwrap();
    bridge.quit();
    assert!(matches!(
        bridge
            .observe(
                &envelope("idle", "w9:p5", 31, 8),
                LifecycleReliability::Verified
            )
            .unwrap()
            .as_slice(),
        [BridgeEffect::SendFeedback { .. }]
    ));
}

#[test]
fn output_revision_increases_preserve_queued_review_until_verified_idle() {
    let mut bridge = bridge();
    bridge
        .observe(
            &envelope("blocked", "w9:p5", 7, 6),
            LifecycleReliability::Verified,
        )
        .unwrap();
    bridge.submit(report()).unwrap();
    for revision in [327, 753, 1102] {
        assert!(
            bridge
                .observe(
                    &envelope("blocked", "w9:p5", revision, 6),
                    LifecycleReliability::Verified
                )
                .unwrap()
                .is_empty()
        );
    }
    assert!(
        matches!(bridge.observe(&envelope("idle", "w9:p5", 1103, 7), LifecycleReliability::Verified).unwrap().as_slice(), [BridgeEffect::SendFeedback { report }] if report.snapshot_id == "snapshot-17")
    );
}

#[test]
fn changed_terminal_identity_invalidates_the_previous_sessions_pending_review() {
    let mut bridge = bridge();
    bridge
        .observe(
            &envelope("blocked", "w9:p5", 31, 6),
            LifecycleReliability::Verified,
        )
        .unwrap();
    bridge.submit(report()).unwrap();
    let invalidation = bridge
        .observe(
            &envelope_on_terminal("working", "w9:p5", 32, 1, "term_restarted"),
            LifecycleReliability::Verified,
        )
        .unwrap();
    assert!(
        matches!(invalidation.as_slice(), [BridgeEffect::InvalidateReview { snapshot_id }] if snapshot_id == "snapshot-17")
    );
    assert!(
        bridge
            .observe(
                &envelope_on_terminal("idle", "w9:p5", 33, 2, "term_restarted"),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
    assert!(bridge.submit(report()).is_err());
}

#[test]
fn explicit_content_snapshot_change_invalidates_queued_feedback() {
    let mut bridge = bridge();
    bridge
        .observe(
            &envelope("blocked", "w9:p5", 31, 6),
            LifecycleReliability::Verified,
        )
        .unwrap();
    bridge.submit(report()).unwrap();
    let effects = bridge.invalidate_snapshot("snapshot-17");
    assert!(
        matches!(effects.as_slice(), [BridgeEffect::InvalidateReview { snapshot_id }] if snapshot_id == "snapshot-17")
    );
    assert!(
        bridge
            .observe(
                &envelope("idle", "w9:p5", 32, 7),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
    assert!(bridge.submit(report()).is_err());
}

#[test]
fn replacing_agent_in_the_same_terminal_invalidates_the_previous_agents_review() {
    let mut bridge = bridge();
    bridge
        .observe(
            &envelope_with_agent("blocked", "w9:p5", 31, 6, "term_65c87de10d6b710", "codex"),
            LifecycleReliability::Verified,
        )
        .unwrap();
    bridge.submit(report()).unwrap();
    let effects =
        bridge.observe_agent_session(AgentSessionIdentity::Verified("agent-run-two".into()));
    assert!(
        matches!(effects.as_slice(), [BridgeEffect::InvalidateReview { snapshot_id }] if snapshot_id == "snapshot-17")
    );
    assert!(
        bridge
            .observe(
                &envelope_with_agent("idle", "w9:p5", 32, 1, "term_65c87de10d6b710", "omp"),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
    assert!(bridge.submit(report()).is_err());
}

#[test]
fn missing_verified_agent_session_never_auto_focuses_or_sends_queued_feedback() {
    let mut unknown = HerdrBridge::new("w9:p5");
    assert!(
        unknown
            .observe(
                &envelope("working", "w9:p5", 31, 5),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
    assert!(
        unknown
            .observe(
                &envelope("blocked", "w9:p5", 32, 6),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        unknown.explicit_gate(),
        BridgeEffect::OfferReview { .. }
    ));
    assert!(unknown.submit(report()).is_err());

    let mut bridge = bridge();
    bridge
        .observe(
            &envelope("blocked", "w9:p5", 31, 6),
            LifecycleReliability::Verified,
        )
        .unwrap();
    bridge.submit(report()).unwrap();
    assert!(
        bridge
            .observe_agent_session(AgentSessionIdentity::Unverified)
            .is_empty()
    );
    assert!(
        bridge
            .observe(
                &envelope("idle", "w9:p5", 32, 7),
                LifecycleReliability::Verified
            )
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        bridge.explicit_gate(),
        BridgeEffect::OfferReview { .. }
    ));
}
