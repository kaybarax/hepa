use serde_json::{Map, Value};
use std::fs;
use std::path::{Path, PathBuf};

const FIXTURES: &[&str] = &[
    "task_spec.json",
    "fleet_task.json",
    "lane.json",
    "review_report.json",
    "merge_readiness.json",
    "redaction_cases.json",
];

fn fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/hoca-reference")
        .canonicalize()
        .expect("fixture directory exists")
}

fn load_fixture(name: &str) -> (String, Value) {
    let path = fixture_dir().join(name);
    let raw = fs::read_to_string(&path).expect("fixture is readable");
    let parsed = serde_json::from_str(&raw).expect("fixture is valid JSON");
    (raw, parsed)
}

fn object(value: &Value) -> &Map<String, Value> {
    value.as_object().expect("fixture root is a JSON object")
}

fn require_string<'a>(map: &'a Map<String, Value>, field: &str) -> &'a str {
    map.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| panic!("missing non-empty string field: {field}"))
}

fn require_array(map: &Map<String, Value>, field: &str) {
    map.get(field)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("missing array field: {field}"));
}

fn is_secret_like_path(path: &str) -> bool {
    let normalized = path.trim_matches('/').to_ascii_lowercase();
    let name = normalized.rsplit('/').next().unwrap_or("");
    matches!(
        name,
        ".env"
            | ".netrc"
            | ".npmrc"
            | ".pypirc"
            | ".htpasswd"
            | "credentials"
            | "application_default_credentials.json"
            | "cookies"
            | "cookies.sqlite"
            | "id_rsa"
            | "id_ed25519"
    ) || (name.starts_with(".env.") && name != ".env.example")
        || [
            ".jks",
            ".keystore",
            ".key",
            ".pem",
            ".p12",
            ".pfx",
            ".kubeconfig",
        ]
        .iter()
        .any(|suffix| name.ends_with(suffix))
        || [
            ".aws/credentials",
            ".config/gcloud/application_default_credentials.json",
            ".docker/config.json",
            ".github/secrets",
            ".gnupg",
            ".ssh",
            ".azure",
        ]
        .iter()
        .any(|secret| normalized == *secret || normalized.starts_with(&format!("{secret}/")))
        || normalized.split('/').any(|part| {
            matches!(
                part,
                ".aws" | ".azure" | ".docker" | ".gnupg" | ".ssh" | "keychains"
            )
        })
        || [
            "cookies",
            "firefox/profiles",
            "google/chrome/default",
            "brave-browser/default",
            "microsoft edge/default",
        ]
        .iter()
        .any(|cookie_path| normalized.contains(cookie_path))
}

#[test]
fn hoca_reference_fixtures_are_canonical_json() {
    for name in FIXTURES {
        let (raw, parsed) = load_fixture(name);
        let canonical = serde_json::to_string_pretty(&parsed).expect("fixture serializes") + "\n";
        assert_eq!(raw, canonical, "{name} is not canonical pretty JSON");
    }
}

#[test]
fn hoca_task_spec_fixture_has_required_contract_fields() {
    let (_, parsed) = load_fixture("task_spec.json");
    let map = object(&parsed);

    for field in [
        "run_id",
        "repo_root",
        "base_branch",
        "task_branch",
        "raw_request",
        "goal",
        "risk_level",
    ] {
        require_string(map, field);
    }
    for field in [
        "non_goals",
        "expected_areas",
        "acceptance_criteria",
        "test_commands",
    ] {
        require_array(map, field);
    }

    assert!(matches!(
        require_string(map, "risk_level"),
        "low" | "medium" | "high"
    ));
    assert!(map.get("models").and_then(Value::as_object).is_some());
    assert!(map.get("sandbox").and_then(Value::as_object).is_some());
}

#[test]
fn fixture_validation_errors_are_detectable() {
    let (_, mut lane) = load_fixture("lane.json");
    lane["status"] = Value::String("invalid".to_string());
    assert!(!matches!(
        lane["status"].as_str(),
        Some(
            "allocated"
                | "starting"
                | "running"
                | "validating"
                | "reviewing"
                | "repairing"
                | "pr_created"
                | "ready_for_human"
                | "blocked"
                | "failed"
                | "cleaned"
        )
    ));

    let (_, mut review) = load_fixture("review_report.json");
    review["findings"][0]["severity"] = Value::String("tiny".to_string());
    assert!(!matches!(
        review["findings"][0]["severity"].as_str(),
        Some("critical" | "high" | "medium" | "low" | "nit")
    ));

    let (_, mut spec) = load_fixture("task_spec.json");
    spec.as_object_mut().expect("object").remove("run_id");
    assert!(object(&spec).get("run_id").is_none());
}

#[test]
fn safety_boundary_fixtures_use_sanitized_paths_and_secret_markers() {
    let macos_home_path = format!("/{}/", "Users");
    let unix_home_path = format!("/{}/", "home");
    let manager_token_name = ["GITHUB", "TOKEN"].join("_");

    for name in FIXTURES {
        let (raw, _) = load_fixture(name);
        assert!(
            !raw.contains(&macos_home_path),
            "{name} contains a local macOS path"
        );
        assert!(
            !raw.contains(&unix_home_path),
            "{name} contains a local home path"
        );
        assert!(
            !raw.contains("@example."),
            "{name} contains email-shaped fixture data"
        );
        assert!(
            !raw.contains(&manager_token_name),
            "{name} contains a manager credential name"
        );
    }

    let (_, lane) = load_fixture("lane.json");
    let worktree_path = lane["worktree_path"]
        .as_str()
        .expect("worktree path exists");
    assert!(!is_secret_like_path(worktree_path));

    assert!(is_secret_like_path(".env.local"));
    assert!(is_secret_like_path(".ssh/id_ed25519"));
    assert!(!is_secret_like_path(".env.example"));
}

#[test]
fn redaction_case_fixture_records_expected_public_placeholders() {
    let (_, parsed) = load_fixture("redaction_cases.json");
    let cases = parsed["cases"].as_array().expect("cases are present");
    let first = cases.first().expect("at least one redaction case");
    let output = first["output"].as_str().expect("output is a string");

    assert!(output.contains("<LOCAL_PATH>"));
    assert!(output.contains("api_key=<REDACTED>"));
    assert!(output.contains("<EMAIL>"));
}
