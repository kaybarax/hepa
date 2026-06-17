use hepa_core::redaction::redact_secrets;

/// Known leak classes that must never survive redaction before persistence.
/// Each fixture pairs leaking text with the substring that must disappear.
const LEAK_FIXTURES: &[(&str, &str)] = &[
    (
        "GITHUB_TOKEN=ghp_0123456789abcdefABCDEF",
        "ghp_0123456789abcdefABCDEF",
    ),
    (
        "gh auth: github_pat_11ABCDE_secretpartvalue",
        "github_pat_11ABCDE_secretpartvalue",
    ),
    (
        "export OPENAI_API_KEY=sk-proj-abcdefghijklmnop",
        "sk-proj-abcdefghijklmnop",
    ),
    (
        "aws key AKIAIOSFODNN7EXAMPLE in logs",
        "AKIAIOSFODNN7EXAMPLE",
    ),
    (
        "slack token xoxb-12345-67890-abcdef",
        "xoxb-12345-67890-abcdef",
    ),
    (
        "Authorization: Bearer eyJ0b2tlbiI6InNlY3JldCJ9",
        "eyJ0b2tlbiI6InNlY3JldCJ9",
    ),
    ("password: hunter2supersecret", "hunter2supersecret"),
    ("db_credential=postgres://user:p4ssw0rd@host", "p4ssw0rd"),
];

#[test]
fn redaction_catches_known_leak_classes() {
    for (fixture, secret) in LEAK_FIXTURES {
        let redacted = redact_secrets(fixture);
        assert!(
            !redacted.contains(secret),
            "leak survived redaction: fixture={fixture:?} secret={secret:?} -> {redacted:?}"
        );
        assert!(
            redacted.contains("<redacted>"),
            "expected a redaction marker for fixture {fixture:?}, got {redacted:?}"
        );
    }
}

#[test]
fn redaction_leaves_benign_text_untouched() {
    let benign = "This PR updates the README and adds a test for the login redirect.";
    assert_eq!(redact_secrets(benign), benign);
}
