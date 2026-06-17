/// The placeholder substituted for any detected secret.
pub const REDACTED: &str = "<redacted>";

/// Known secret token prefixes redacted wherever they appear.
const TOKEN_PREFIXES: &[&str] = &[
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "sk-",
    "AKIA",
];

/// Key fragments (case-insensitive) that mark a `key=value` / `key: value`
/// assignment as secret-bearing.
const SECRET_KEY_FRAGMENTS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "api_key",
    "apikey",
    "access_key",
    "private_key",
    "credential",
    "authorization",
];

/// Redact secrets from arbitrary text before it is persisted to prompts, logs,
/// artifacts, card comments, diagnostics, or PR bodies. Redaction is
/// deterministic and idempotent.
pub fn redact_secrets(text: &str) -> String {
    let mut out = text.lines().map(redact_line).collect::<Vec<_>>().join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn redact_line(line: &str) -> String {
    if let Some(redacted) = redact_key_value(line) {
        return redact_tokens(&redacted);
    }
    redact_tokens(line)
}

fn redact_key_value(line: &str) -> Option<String> {
    let separator = line.find(['=', ':'])?;
    let key = line[..separator].trim().to_ascii_lowercase();
    let key = key.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
    if SECRET_KEY_FRAGMENTS
        .iter()
        .any(|fragment| key.contains(fragment))
    {
        let prefix = &line[..=separator];
        let spacing = if line[separator + 1..].starts_with(' ') {
            " "
        } else {
            ""
        };
        return Some(format!("{prefix}{spacing}{REDACTED}"));
    }
    None
}

fn redact_tokens(line: &str) -> String {
    let mut result = String::with_capacity(line.len());
    let mut previous_was_bearer = false;
    for (index, token) in line.split(' ').enumerate() {
        if index > 0 {
            result.push(' ');
        }
        if previous_was_bearer && !token.is_empty() {
            result.push_str(REDACTED);
            previous_was_bearer = false;
            continue;
        }
        if token.eq_ignore_ascii_case("bearer") {
            result.push_str(token);
            previous_was_bearer = true;
            continue;
        }
        result.push_str(&redact_token(token));
    }
    result
}

fn redact_token(token: &str) -> String {
    // Preserve surrounding punctuation, redacting only the secret-like core.
    let core_start = token.find(|c: char| c.is_ascii_alphanumeric()).unwrap_or(0);
    let core_end = token
        .rfind(|c: char| c.is_ascii_alphanumeric())
        .map(|index| index + 1)
        .unwrap_or(token.len());
    if core_start >= core_end {
        return token.to_string();
    }
    let core = &token[core_start..core_end];
    if TOKEN_PREFIXES
        .iter()
        .any(|prefix| core.starts_with(prefix) && core.len() > prefix.len())
    {
        format!("{}{REDACTED}{}", &token[..core_start], &token[core_end..])
    } else {
        token.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_key_value_secret_assignments() {
        let input = "GITHUB_TOKEN=ghp_supersecretvalue\nPASSWORD: hunter2\nNOTE: keep me";
        let output = redact_secrets(input);

        assert!(!output.contains("ghp_supersecretvalue"));
        assert!(!output.contains("hunter2"));
        assert!(output.contains("GITHUB_TOKEN=<redacted>"));
        assert!(output.contains("PASSWORD: <redacted>"));
        // Non-secret lines are preserved.
        assert!(output.contains("NOTE: keep me"));
    }

    #[test]
    fn redacts_known_token_prefixes_inline() {
        let input = "cloned with token ghp_abcdef1234567890 from remote";
        let output = redact_secrets(input);
        assert!(!output.contains("ghp_abcdef1234567890"));
        assert!(output.contains("<redacted>"));
    }

    #[test]
    fn redacts_bearer_authorization_headers() {
        let input = "Authorization: Bearer abc123def456";
        let output = redact_secrets(input);
        assert!(!output.contains("abc123def456"));
    }

    #[test]
    fn redaction_is_idempotent_and_preserves_trailing_newline() {
        let input = "api_key=sk-deadbeef\n";
        let once = redact_secrets(input);
        let twice = redact_secrets(&once);
        assert_eq!(once, twice);
        assert!(once.ends_with('\n'));
        assert!(!once.contains("sk-deadbeef"));
    }
}
