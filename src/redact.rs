//! Taking secrets out of text read from transcripts, before it's indexed for
//! search or shown. People paste keys into sessions, and tools print tokens.
//!
//! A secret is found by its shape: a provider's key prefix followed by enough
//! key characters that it can't be an ordinary word (`sk-` alone is in
//! `sk-learn`), a JSON Web Token's three dotted parts, whatever follows
//! `Bearer `, or a private key's PEM block. A scanner rather than regular
//! expressions: one pass, and nothing a crafted transcript can make slow.

/// Key prefixes, each with how many key characters must follow. `sk-` covers
/// Anthropic's `sk-ant-`, OpenRouter's `sk-or-` and OpenAI's `sk-proj-`.
const PREFIXES: &[(&str, usize)] = &[
    ("sk-", 20),
    ("xai-", 20),
    ("ghp_", 20),
    ("gho_", 20),
    ("ghu_", 20),
    ("ghs_", 20),
    ("ghr_", 20),
    ("github_pat_", 20),
    ("glpat-", 20),
    ("AKIA", 16),
    ("AIza", 30),
];

/// `text` with every secret in it replaced by `[redacted]`.
pub fn redact(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut copied = 0;
    let mut at = 0;
    while at < bytes.len() {
        let boundary = at == 0 || !key_byte(bytes[at - 1]);
        match boundary.then(|| secret_at(bytes, at)).flatten() {
            Some((start, end)) => {
                out.push_str(&text[copied..start]);
                out.push_str("[redacted]");
                (copied, at) = (end, end);
            }
            None => at += 1,
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// Where the secret starting at `at` begins and ends: after `Bearer `, the
/// token alone.
fn secret_at(bytes: &[u8], at: usize) -> Option<(usize, usize)> {
    let rest = &bytes[at..];
    let run = |bytes: &[u8], wanted: &dyn Fn(u8) -> bool| {
        bytes.iter().take_while(|&&byte| wanted(byte)).count()
    };
    if let Some(after) = rest
        .strip_prefix(b"Bearer ")
        .or_else(|| rest.strip_prefix(b"bearer "))
    {
        let token = run(after, &|byte| key_byte(byte) || b"./+=".contains(&byte));
        return (token >= 20).then_some((at + 7, at + 7 + token));
    }
    if rest.starts_with(b"-----BEGIN ") {
        // A private key's block, to its END line, which a key's is within a
        // few kilobytes of: looked for no further, so it stays one pass.
        let block = &rest[..rest.len().min(16_384)];
        let header = block.iter().position(|&byte| byte == b'\n')?;
        if !block[..header]
            .windows(11)
            .any(|word| word == b"PRIVATE KEY")
        {
            return None;
        }
        let end = block.windows(9).position(|word| word == b"-----END ")? + 9;
        let close = block[end..].windows(5).position(|word| word == b"-----")? + end + 5;
        return Some((at, at + close));
    }
    if rest.starts_with(b"eyJ") {
        // Three base64url parts joined by dots, each of ten characters at least.
        let mut length = 0;
        for part in 0..3 {
            if part > 0 {
                if rest.get(length) != Some(&b'.') {
                    return None;
                }
                length += 1;
            }
            let part = run(&rest[length..], &key_byte);
            if part < 10 {
                return None;
            }
            length += part;
        }
        return Some((at, at + length));
    }
    PREFIXES.iter().find_map(|(prefix, least)| {
        let key = run(rest.strip_prefix(prefix.as_bytes())?, &key_byte);
        (key >= *least).then_some((at, at + prefix.len() + key))
    })
}

fn key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn keys_of_known_shapes_are_redacted_and_words_like_them_are_not() {
        let text = "export OPENROUTER_API_KEY=sk-or-v1-0123456789abcdef0123456789abcdef and \
                    ANTHROPIC=sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAA; pip install sk-learn";
        assert_eq!(
            redact(text),
            "export OPENROUTER_API_KEY=[redacted] and ANTHROPIC=[redacted]; pip install sk-learn"
        );
        assert_eq!(
            redact("curl -H 'Authorization: Bearer abcdefghijklmnopqrstuvwxyz012345' x"),
            "curl -H 'Authorization: Bearer [redacted]' x"
        );
        assert_eq!(
            redact("token eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghijklmnop end"),
            "token [redacted] end"
        );
        assert_eq!(redact("AKIAIOSFODNN7EXAMPLE"), "[redacted]");
        assert_eq!(
            redact("task-0123456789abcdefghij0123"),
            "task-0123456789abcdefghij0123"
        );
        assert_eq!(redact("plain text, ünïcödé"), "plain text, ünïcödé");
        assert_eq!(
            redact("authorization: bearer abcdefghijklmnopqrstuvwxyz012345"),
            "authorization: bearer [redacted]"
        );
        assert_eq!(
            redact(
                "cat id_ed25519\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk\n-----END OPENSSH PRIVATE KEY-----\ndone"
            ),
            "cat id_ed25519\n[redacted]\ndone"
        );
        assert_eq!(
            redact("-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----"),
            "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----"
        );
    }
}
