use super::*;

fn is_det(failure: &CompactFailure) -> bool {
    matches!(failure, CompactFailure::Deterministic(_))
}

#[test]
fn sampling_api_4xx_is_deterministic_except_408_and_429() {
    let det = |s: StatusCode| {
        is_det(&classify_sampling_error(SamplingError::Api {
            status: s,
            message: "test".into(),
            model_metadata: None,
            retry_after_secs: None,
            should_retry: None,
            error_code: None,
        }))
    };
    assert!(det(StatusCode::BAD_REQUEST));
    assert!(det(StatusCode::UNAUTHORIZED));
    assert!(det(StatusCode::FORBIDDEN));
    assert!(det(StatusCode::NOT_FOUND));
    assert!(det(StatusCode::PAYLOAD_TOO_LARGE));
    assert!(!det(StatusCode::REQUEST_TIMEOUT));
    assert!(!det(StatusCode::TOO_MANY_REQUESTS));
    assert!(!det(StatusCode::INTERNAL_SERVER_ERROR));
    assert!(!det(StatusCode::BAD_GATEWAY));
    assert!(!det(StatusCode::SERVICE_UNAVAILABLE));
}

#[test]
fn sampling_non_api_variants_classify_correctly() {
    assert!(is_det(&classify_sampling_error(
        SamplingError::auth_unknown("expired")
    )));
    assert!(is_det(&classify_sampling_error(
        SamplingError::InvalidConfiguration("missing key")
    )));
    assert!(is_det(&classify_sampling_error(
        SamplingError::IdleTimeout { elapsed_secs: 60 }
    )));
    assert!(!is_det(&classify_sampling_error(
        SamplingError::EventStreamError("conn reset".into())
    )));
    assert!(!is_det(&classify_sampling_error(
        SamplingError::StreamError {
            error_type: "overloaded_error".into(),
            message: "try again".into(),
            code: None,
        }
    )));
}

#[test]
fn response_event_invalid_request_error_marker_is_deterministic() {
    // The documented schema-violation marker for the Anthropic Messages API. Production
    // `messages.X.content.Y: thinking blocks ...` errors take this branch.
    assert!(is_det(&classify_response_event_error(
        Some("invalid_request_error"),
        "messages.27.content.1: ..."
    )));
    // Marker can also appear in the message body (e.g. wrapped error
    // envelopes from gateways).
    assert!(is_det(&classify_response_event_error(
        Some("400"),
        "Provider returned invalid_request_error: messages.X..."
    )));
}

#[test]
fn response_event_numeric_codes_match_http_classification() {
    let det = |c: &str| is_det(&classify_response_event_error(Some(c), "msg"));
    assert!(det("400"));
    assert!(det("401"));
    assert!(det("403"));
    assert!(det("404"));
    assert!(!det("408"));
    assert!(!det("429"));
    assert!(!det("500"));
    assert!(!det("503"));
}

#[test]
fn response_event_unknown_code_defaults_to_transient() {
    // Default-to-retry on uncertainty: unparseable, none, and unrecognized
    // string codes all surface as Transient so we don't swallow blips.
    assert!(!is_det(&classify_response_event_error(None, "msg")));
    assert!(!is_det(&classify_response_event_error(
        Some("error"),
        "msg"
    )));
    assert!(!is_det(&classify_response_event_error(
        Some("overloaded_error"),
        "msg"
    )));
}

#[test]
fn response_event_marker_in_message_with_no_code_is_deterministic() {
    // The most permissive shape a Anthropic Messages API might emit: no structured code
    // field, schema-violation marker only in the human-readable message.
    assert!(is_det(&classify_response_event_error(
        None,
        "messages.X.content.Y: invalid_request_error: ..."
    )));
}

#[test]
fn response_event_context_length_message_is_deterministic() {
    // The inference backend streams the size overflow as a ResponseError with no usable code
    // (`code="none"`); only the message identifies it, and it must be deterministic.
    assert!(is_det(&classify_response_event_error(
        None,
        "The prompt is too long for this model's context window."
    )));
}

#[test]
fn sampling_api_500_with_context_length_message_is_deterministic() {
    // The sampler synthesizes status=500 from a streamed size overflow, so
    // status alone reads transient; the message must still short-circuit.
    assert!(is_det(&classify_sampling_error(SamplingError::Api {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: "API error (status 500 Internal Server Error): \
                  The prompt is too long for this model's context window."
            .into(),
        model_metadata: None,
        retry_after_secs: None,
        should_retry: None,
        error_code: None,
    })));
}

#[test]
fn sampling_http_is_transient() {
    // reqwest::Error has no public constructor; trigger one via a known-bad
    // request. reqwest's TCP connect needs a Tokio reactor — futures::executor
    // is not enough (CI runs in a Bazel sandbox where the failure surfaces).
    // Tests the SamplingError::Http -> Transient branch.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let http_err = rt
        .block_on(reqwest::get("http://127.0.0.1:0"))
        .expect_err("connecting to port 0 must fail");
    assert!(!is_det(&classify_sampling_error(SamplingError::Http(
        http_err
    ))));
}

#[test]
fn sampling_serialization_is_deterministic() {
    let serde_err = serde_json::from_str::<u32>("not a number").unwrap_err();
    assert!(is_det(&classify_sampling_error(
        SamplingError::Serialization(serde_err)
    )));
}

#[test]
fn classifier_preserves_acp_error_data() {
    // Inner `acp::Error` survives the wrap; data field carries the
    // upstream rendering. Both Deterministic and Transient arms must
    // route the inner error through the variant payload.
    let CompactFailure::Deterministic(err) = classify_sampling_error(SamplingError::Api {
        status: StatusCode::BAD_REQUEST,
        message: "bad payload".into(),
        model_metadata: None,
        retry_after_secs: None,
        should_retry: None,
        error_code: None,
    }) else {
        panic!("expected Deterministic for 400");
    };
    let data = err.data.as_ref().and_then(|d| d.as_str()).unwrap();
    assert!(data.contains("compact failed"));
    assert!(data.contains("bad payload"));

    let CompactFailure::Transient(err) = classify_sampling_error(SamplingError::Api {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        message: "upstream blip".into(),
        model_metadata: None,
        retry_after_secs: None,
        should_retry: None,
        error_code: None,
    }) else {
        panic!("expected Transient for 500");
    };
    let data = err.data.as_ref().and_then(|d| d.as_str()).unwrap();
    assert!(data.contains("upstream blip"));
}

#[test]
fn stream_timing_boundaries() {
    let mut t = StreamTiming::new();
    assert_eq!(t.count, 0);
    assert_eq!(t.ttft_ms(), None);
    assert_eq!(t.stream_ms(), None);
    assert_eq!(t.itl_max_ms(), None);
    t.record_delta();
    assert_eq!(t.count, 1);
    assert!(t.ttft_ms().is_some());
    assert!(t.stream_ms().is_some());
    assert_eq!(t.itl_max_ms(), None); // need >= 2 deltas for a gap
    t.record_delta();
    assert_eq!(t.count, 2);
    assert!(t.itl_max_ms().is_some());
}

#[test]
fn compaction_outcome_as_str_is_stable() {
    assert_eq!(CompactionOutcome::Success.as_str(), "success");
    assert_eq!(CompactionOutcome::Truncated.as_str(), "truncated");
    assert_eq!(CompactionOutcome::Deterministic.as_str(), "deterministic");
    assert_eq!(CompactionOutcome::Transient.as_str(), "transient");
    assert_eq!(CompactionOutcome::Degenerate.as_str(), "degenerate");
    assert_eq!(CompactionOutcome::Failed.as_str(), "failed");
}
