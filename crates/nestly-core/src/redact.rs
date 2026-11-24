use std::sync::OnceLock;

use aho_corasick::AhoCorasick;
use regex::Regex;

const REDACTED: &str = "***";

/// Scrubs secrets from log lines, alerts, and error messages.
///
/// Exact secret values are matched first, then well-known credential shapes.
/// The redactor never retains the secret values after construction.
#[derive(Debug, Clone)]
pub struct Redactor {
    exact: Option<AhoCorasick>,
    exact_replacements: Vec<&'static str>,
    patterns: &'static [Regex],
}

impl Redactor {
    /// Build a redactor from all known secret values.
    #[must_use]
    pub fn new<I, S>(secrets: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut values: Vec<String> = secrets
            .into_iter()
            .map(|value| value.as_ref().to_owned())
            .filter(|value| value.len() >= 8)
            .collect();
        values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        values.dedup();

        let replacement_count = values.len();
        let exact = if values.is_empty() {
            None
        } else {
            AhoCorasick::builder()
                .match_kind(aho_corasick::MatchKind::LeftmostLongest)
                .build(values)
                .ok()
        };

        Self {
            exact,
            exact_replacements: vec![REDACTED; replacement_count],
            patterns: patterns(),
        }
    }

    /// Replace every known secret and credential-shaped token with `***`.
    #[must_use]
    pub fn redact(&self, input: &str) -> String {
        let mut output = match &self.exact {
            Some(ac) => ac.replace_all(input, &self.exact_replacements),
            None => input.to_owned(),
        };

        for pattern in self.patterns {
            output = pattern.replace_all(&output, REDACTED).into_owned();
        }
        output
    }

    /// Redact a single line and cap its length.
    #[must_use]
    pub fn redact_line(&self, input: &str, max_chars: usize) -> String {
        let redacted = self.redact(input);
        if redacted.chars().count() <= max_chars {
            redacted
        } else {
            let mut truncated: String = redacted.chars().take(max_chars).collect();
            truncated.push_str("...");
            truncated
        }
    }
}

fn patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            // Discord bot/user tokens: base64-ish segments separated by dots.
            Regex::new(r"[A-Za-z0-9_-]{24,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{27,}")
                .expect("static token regex"),
            // MFA tokens.
            Regex::new(r"mfa\.[A-Za-z0-9_-]{20,}").expect("static mfa regex"),
            // Authorization headers in any casing.
            Regex::new(r"(?i)(authorization\s*:\s*(?:bot|bearer)?\s*)\S+")
                .expect("static authorization regex"),
            // token=... and access_token=... in query strings or logs.
            Regex::new(r"(?i)((?:access_)?token=)[^\s&]+").expect("static query token regex"),
            // Webhook URLs contain a credential in the path.
            Regex::new(
                r"https://(?:ptb\.|canary\.)?discord(?:app)?\.com/api/webhooks/\d+/[A-Za-z0-9_-]+",
            )
            .expect("static webhook regex"),
        ]
    })
}

#[cfg(test)]
mod tests {
    use super::Redactor;

    #[test]
    fn redacts_exact_secret() {
        let secret = "super-secret-value-12345";
        let redactor = Redactor::new([secret]);
        let output = redactor.redact(&format!("token is {secret} ok"));
        assert!(!output.contains(secret));
        assert!(output.contains("***"));
    }

    #[test]
    fn redacts_token_shapes() {
        let redactor = Redactor::new(Vec::<String>::new());
        let token = "MTAxMjM0NTY3ODkwMTIzNDU2.GaBcDe.abcdefghijklmnopqrstuvwxyz123456";
        let output = redactor.redact(&format!("Authorization: Bot {token}"));
        assert!(!output.contains(token));
        assert!(!output.contains("Bot MTA"));
    }

    #[test]
    fn redacts_query_tokens_and_webhooks() {
        let redactor = Redactor::new(Vec::<String>::new());
        let output = redactor
            .redact("GET /x?access_token=abc123&x=1 https://discord.com/api/webhooks/123/abcDEF");
        assert!(!output.contains("abc123"));
        assert!(!output.contains("webhooks/123/abcDEF"));
    }

    #[test]
    fn caps_long_lines() {
        let redactor = Redactor::new(Vec::<String>::new());
        let output = redactor.redact_line(&"a".repeat(100), 10);
        assert_eq!(output.chars().count(), 13);
        assert!(output.ends_with("..."));
    }

    #[test]
    fn ignores_short_secrets() {
        let redactor = Redactor::new(["abc"]);
        assert_eq!(redactor.redact("abc"), "abc");
    }
}
