use hepa_core::contracts::{HepaReviewSignal, HepaValidate};
use std::{error::Error, fmt, sync::Arc, thread};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaReviewFanoutInput {
    pub lane_id: String,
    pub diff_context: String,
    pub validation_summary: String,
    pub max_diff_bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaReviewFanoutRequest {
    pub lane_id: String,
    pub adapter_id: String,
    pub diff_context: String,
    pub validation_summary: String,
}

#[derive(Clone)]
pub struct HepaConfiguredReviewer {
    pub adapter_id: String,
    review: Arc<ReviewerFn>,
}

type ReviewerFn = dyn Fn(HepaReviewFanoutRequest) -> Result<HepaReviewSignal, HepaReviewFanoutError>
    + Send
    + Sync
    + 'static;

impl HepaConfiguredReviewer {
    pub fn new(
        adapter_id: impl Into<String>,
        review: impl Fn(HepaReviewFanoutRequest) -> Result<HepaReviewSignal, HepaReviewFanoutError>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            adapter_id: adapter_id.into(),
            review: Arc::new(review),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaReviewFanoutResult {
    pub lane_id: String,
    pub signals: Vec<HepaReviewSignal>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HepaReviewFanoutError {
    pub field: String,
    pub message: String,
}

impl HepaReviewFanoutError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for HepaReviewFanoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.field, self.message)
    }
}

impl Error for HepaReviewFanoutError {}

pub fn run_configured_reviewers_concurrently(
    input: HepaReviewFanoutInput,
    reviewers: Vec<HepaConfiguredReviewer>,
) -> Result<HepaReviewFanoutResult, HepaReviewFanoutError> {
    validate_input(&input, &reviewers)?;
    let capped_diff = cap_utf8(&input.diff_context, input.max_diff_bytes);
    let mut handles = Vec::with_capacity(reviewers.len());
    for reviewer in reviewers {
        let request = HepaReviewFanoutRequest {
            lane_id: input.lane_id.clone(),
            adapter_id: reviewer.adapter_id.clone(),
            diff_context: capped_diff.clone(),
            validation_summary: input.validation_summary.clone(),
        };
        handles.push(thread::spawn(move || (reviewer.review)(request)));
    }

    let mut signals = Vec::with_capacity(handles.len());
    for handle in handles {
        let signal = handle
            .join()
            .map_err(|_| HepaReviewFanoutError::new("reviewer", "reviewer thread panicked"))??;
        signal
            .validate()
            .map_err(|error| HepaReviewFanoutError::new(error.field, error.message))?;
        signals.push(signal);
    }
    signals.sort_by(|left, right| {
        left.adapter_id
            .cmp(&right.adapter_id)
            .then_with(|| left.review_id.cmp(&right.review_id))
    });

    Ok(HepaReviewFanoutResult {
        lane_id: input.lane_id,
        signals,
    })
}

fn validate_input(
    input: &HepaReviewFanoutInput,
    reviewers: &[HepaConfiguredReviewer],
) -> Result<(), HepaReviewFanoutError> {
    require_single_line("lane_id", &input.lane_id)?;
    require_single_line("validation_summary", &input.validation_summary)?;
    if reviewers.is_empty() {
        return Err(HepaReviewFanoutError::new(
            "reviewers",
            "at least one reviewer is required",
        ));
    }
    for (index, reviewer) in reviewers.iter().enumerate() {
        require_single_line(
            format!("reviewers[{index}].adapter_id"),
            &reviewer.adapter_id,
        )?;
    }
    Ok(())
}

fn require_single_line(field: impl Into<String>, value: &str) -> Result<(), HepaReviewFanoutError> {
    let field = field.into();
    if value.trim().is_empty() {
        return Err(HepaReviewFanoutError::new(field, "must not be empty"));
    }
    if value.contains('\n') || value.contains('\r') {
        return Err(HepaReviewFanoutError::new(field, "must be a single line"));
    }
    Ok(())
}

fn cap_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = 0;
    for (index, character) in value.char_indices() {
        let next_end = index + character.len_utf8();
        if next_end > max_bytes {
            break;
        }
        end = next_end;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hepa_core::contracts::{CONTRACT_SCHEMA_VERSION, HepaReviewStatus, HepaValidationStatus};
    use std::{
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };

    #[test]
    fn configured_reviewers_run_concurrently_with_capped_context() {
        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let reviewers = ["reviewer-b", "reviewer-a"]
            .into_iter()
            .map(|adapter_id| {
                let active = Arc::clone(&active);
                let max_active = Arc::clone(&max_active);
                let seen = Arc::clone(&seen);
                HepaConfiguredReviewer::new(adapter_id, move |request| {
                    let now_active = active.fetch_add(1, Ordering::SeqCst) + 1;
                    max_active.fetch_max(now_active, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(120));
                    active.fetch_sub(1, Ordering::SeqCst);
                    seen.lock().expect("seen lock").push((
                        request.adapter_id.clone(),
                        request.diff_context.clone(),
                        request.validation_summary.clone(),
                    ));
                    Ok(signal(&request.lane_id, &request.adapter_id))
                })
            })
            .collect::<Vec<_>>();
        let started = Instant::now();

        let result = run_configured_reviewers_concurrently(
            HepaReviewFanoutInput {
                lane_id: "lane-1".to_string(),
                diff_context: "0123456789abcdef".to_string(),
                validation_summary: stable_json_name(HepaValidationStatus::Passed),
                max_diff_bytes: 8,
            },
            reviewers,
        )
        .expect("fanout should complete");

        assert!(
            started.elapsed() < Duration::from_millis(220),
            "fanout should be closer to one reviewer than serial execution"
        );
        assert_eq!(max_active.load(Ordering::SeqCst), 2);
        assert_eq!(
            result
                .signals
                .iter()
                .map(|signal| signal.adapter_id.as_str())
                .collect::<Vec<_>>(),
            vec!["reviewer-a", "reviewer-b"]
        );
        let seen = seen.lock().expect("seen lock");
        assert_eq!(seen.len(), 2);
        for (_, diff_context, validation_summary) in seen.iter() {
            assert_eq!(diff_context, "01234567");
            assert_eq!(validation_summary, "passed");
        }
    }

    fn signal(lane_id: &str, adapter_id: &str) -> HepaReviewSignal {
        HepaReviewSignal {
            schema_version: CONTRACT_SCHEMA_VERSION,
            review_id: format!("review-{adapter_id}"),
            lane_id: lane_id.to_string(),
            adapter_id: adapter_id.to_string(),
            status: HepaReviewStatus::Approved,
            findings: Vec::new(),
            summary: vec!["approved".to_string()],
            completed_at: "2026-06-16T00:00:00Z".to_string(),
        }
    }

    fn stable_json_name(status: HepaValidationStatus) -> String {
        match status {
            HepaValidationStatus::Passed => "passed",
            HepaValidationStatus::Failed => "failed",
            HepaValidationStatus::Skipped => "skipped",
            HepaValidationStatus::NoTestsDetected => "no_tests_detected",
        }
        .to_string()
    }
}
