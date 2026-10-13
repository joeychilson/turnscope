//! Taking secrets out of text read from transcripts, before it's indexed for
//! search or shown. People paste keys into sessions, and tools print tokens.
//!
//! A secret is found by its shape: a provider's key prefix followed by enough
//! key characters that it can't be an ordinary word (`sk-` alone is in
//! `sk-learn`), a JSON Web Token's three dotted parts, whatever follows
//! `Bearer `, or a private key's PEM block, even cut short.
//!
//! A scanner rather than regular expressions, so nothing a crafted
//! transcript holds can make it slow. It reads the text once, and from a
//! private key's header looks ahead for the END line: at most 16 KB, and
//! only to the next `-----`, which comes no later than the next header, so
//! no text is looked through for two keys.

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
        // A secret can follow a key byte: the letter of a JSON string's `\n`,
        // `\t` or `\r`, as in a tool call's input, or for a key's block, the
        // `-` of a line a diff removes.
        let escape = matches!(bytes[..at], [.., b'\\', b'n' | b't' | b'r']);
        let boundary = at == 0
            || !key_byte(bytes[at - 1])
            || escape
            || bytes[at..].starts_with(b"-----BEGIN ");
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
        // A private key's block, to its END line. The header's label says
        // it's a private key, rather than the line it's on: a JSON string
        // writes line breaks as `\n`, so a key there has no lines. The END
        // line is the first `-----` after the header, within 16 KB, as a
        // key's is. Without one, as where `head` or a tool's limit cut the
        // key short, the block runs as far as key text does, rather than the
        // key being left whole. Looking past another `-----` for the END
        // line read 16 KB for each header: 5 to 7 seconds for 10 MB of
        // headers, against under 50 ms stopping at the first (October 2026).
        let block = &rest[..rest.len().min(16_384)];
        let dashes = |from: usize, within: usize| {
            block[from..block.len().min(from + within)]
                .windows(5)
                .position(|word| word == b"-----")
                .map(|found| from + found)
        };
        let label = dashes(11, 64)?;
        if !block[11..label]
            .windows(11)
            .any(|word| word == b"PRIVATE KEY")
        {
            return None;
        }
        let header = label + 5;
        let close = dashes(header, block.len())
            // A diff's `-` before the END line makes six dashes.
            .map(|end| end + run(&block[end + 5..], &|byte| byte == b'-'))
            .filter(|&end| block[end..].starts_with(b"-----END "))
            .and_then(|end| dashes(end + 9, 64));
        let length = match close {
            Some(close) => close + 5,
            None => header + key_text(&block[header..]),
        };
        return Some((at, at + length));
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

/// How far a key's text runs at the start of `bytes`: base64, line breaks
/// as written or as a JSON string escapes them, the indentation or line
/// number that starts a line and the spaces that end one, and before the
/// base64, header lines with a `:`, as an encrypted key's `Proc-Type` or a
/// PGP key's `Version`. A blank line ends it once base64 has begun, as only
/// header lines have one after them. It ends after the last base64, so a
/// line break before what follows stays.
fn key_text(bytes: &[u8]) -> usize {
    let line_end = |from: usize| {
        (from..bytes.len())
            .find(|&at| matches!(bytes[at..], [b'\n', ..] | [b'\\', b'n', ..]))
            .unwrap_or(bytes.len())
    };
    let (mut at, mut end, mut indent) = (0, 0, false);
    loop {
        match bytes[at..] {
            [b'\n', ..] | [b'\\', b'n', ..] if indent && end > 0 => return end,
            [b'\n', ..] => (at, indent) = (at + 1, true),
            [b'\\', b'n', ..] => (at, indent) = (at + 2, true),
            [b'\r', ..] => at += 1,
            [b'\\', b'r', ..] => at += 2,
            [b' ' | b'\t' | b'0'..=b'9', ..] if indent => at += 1,
            [b' ' | b'\t', ..] => {
                // Spaces a paste left at a line's end are part of the key.
                // Within a line they end it, so prose after a key stays.
                let spaces = bytes[at..]
                    .iter()
                    .take_while(|&&byte| byte == b' ' || byte == b'\t')
                    .count();
                if !matches!(
                    bytes[at + spaces..],
                    [b'\n' | b'\r', ..] | [b'\\', b'n' | b'r', ..]
                ) {
                    return end;
                }
                at += spaces;
            }
            _ if indent && end == 0 && bytes[at..line_end(at)].contains(&b':') => {
                at = line_end(at);
            }
            [byte, ..] if byte.is_ascii_alphanumeric() || b"+/=".contains(&byte) => {
                (at, end, indent) = (at + 1, at + 1, false);
            }
            _ => return end,
        }
    }
}

fn key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

#[cfg(test)]
mod tests {
    use super::{PREFIXES, redact};

    #[test]
    fn keys_of_known_shapes_are_redacted_and_words_like_them_are_not() {
        for (text, redacted) in [
            (
                "export OPENROUTER_API_KEY=sk-or-v1-0123456789abcdef0123456789abcdef and \
                    ANTHROPIC=sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAAAA; pip install sk-learn",
                "export OPENROUTER_API_KEY=[redacted] and ANTHROPIC=[redacted]; pip install sk-learn",
            ),
            (
                "curl -H 'Authorization: Bearer abcdefghijklmnopqrstuvwxyz012345' x",
                "curl -H 'Authorization: Bearer [redacted]' x",
            ),
            (
                "token eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghijklmnop end",
                "token [redacted] end",
            ),
            ("AKIAIOSFODNN7EXAMPLE", "[redacted]"),
            (
                "task-0123456789abcdefghij0123",
                "task-0123456789abcdefghij0123",
            ),
            ("plain text, ünïcödé", "plain text, ünïcödé"),
            (
                "authorization: bearer abcdefghijklmnopqrstuvwxyz012345",
                "authorization: bearer [redacted]",
            ),
            (
                "cat id_ed25519\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk\n-----END OPENSSH PRIVATE KEY-----\ndone",
                "cat id_ed25519\n[redacted]\ndone",
            ),
            (
                "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----",
                "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----",
            ),
            (
                "-----BEGIN PGP PRIVATE KEY BLOCK-----\n\nlQOYBGZha2UAAAAA\n=AAAA\n-----END PGP PRIVATE KEY BLOCK-----",
                "[redacted]",
            ),
        ] {
            assert_eq!(redact(text), redacted, "{text}");
        }
    }

    #[test]
    fn a_key_read_with_line_numbers_is_redacted() {
        for (text, redacted) in [
            (
                "     1\t-----BEGIN OPENSSH PRIVATE KEY-----\n     2\tb3BlbnNzaC1rZXktdjEAAAAAb3BlbnNzaC1rZXktdjEAAAAA\n     3\t-----END OPENSSH PRIVATE KEY-----\n     4\t",
                "     1\t[redacted]\n     4\t",
            ),
            (
                "     1\t-----BEGIN OPENSSH PRIVATE KEY-----\n     2\tb3BlbnNzaC1rZXktdjEAAAAAb3BlbnNzaC1rZXktdjEAAAAA\n     3\tb3BlbnNzaC1rZXktdjEAAAAA\n     4\t",
                "     1\t[redacted]\n     4\t",
            ),
            (
                "     1\t-----BEGIN PGP PRIVATE KEY BLOCK-----\n     2\t\n     3\tlQOYBGZha2UAAAAAlQOYBGZha2UAAAAA\n     4\tlQOYBGZha2UAAAAA",
                "     1\t[redacted]",
            ),
        ] {
            assert_eq!(redact(text), redacted, "{text}");
        }
    }

    #[test]
    fn a_key_in_a_diff_is_redacted() {
        for (text, redacted) in [
            (
                "@@ -1,3 +0,0 @@\n------BEGIN OPENSSH PRIVATE KEY-----\n-b3BlbnNzaC1rZXktdjEAAAAA\n------END OPENSSH PRIVATE KEY-----\n",
                "@@ -1,3 +0,0 @@\n-[redacted]\n",
            ),
            (
                "@@ -0,0 +1,3 @@\n+-----BEGIN OPENSSH PRIVATE KEY-----\n+b3BlbnNzaC1rZXktdjEAAAAA\n+-----END OPENSSH PRIVATE KEY-----\n",
                "@@ -0,0 +1,3 @@\n+[redacted]\n",
            ),
        ] {
            assert_eq!(redact(text), redacted, "{text}");
        }
    }

    #[test]
    fn an_indented_key_is_redacted() {
        for (text, redacted) in [
            (
                "tls:\n  key: |\n    -----BEGIN EC PRIVATE KEY-----\n    MIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n    MIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n    -----END EC PRIVATE KEY-----\n  port: 443",
                "tls:\n  key: |\n    [redacted]\n  port: 443",
            ),
            (
                "key: |\n  -----BEGIN EC PRIVATE KEY-----\n  MIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n  MIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n",
                "key: |\n  [redacted]\n",
            ),
        ] {
            assert_eq!(redact(text), redacted, "{text}");
        }
    }

    #[test]
    fn a_key_in_a_json_string_is_redacted() {
        for (text, redacted) in [
            (
                r#"{"private_key": "-----BEGIN RSA PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n-----END RSA PRIVATE KEY-----\n", "type": "x"}"#,
                r#"{"private_key": "[redacted]\n", "type": "x"}"#,
            ),
            (
                r#"{"content":"-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n-----END PRIVATE KEY-----\n"}"#,
                r#"{"content":"-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n[redacted]\n"}"#,
            ),
            (
                r#""-----BEGIN PRIVATE KEY-----\r\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\r\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC""#,
                r#""[redacted]""#,
            ),
        ] {
            assert_eq!(redact(text), redacted, "{text}");
        }
    }

    #[test]
    fn a_key_cut_short_is_redacted_and_what_follows_it_is_not() {
        for (text, redacted) in [
            (
                "$ head -3 id_rsa\n-----BEGIN RSA PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\r\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n$ ls",
                "$ head -3 id_rsa\n[redacted]\n$ ls",
            ),
            (
                "-----BEGIN RSA PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\r\n\r\nWhy is it cut short?",
                "[redacted]\r\n\r\nWhy is it cut short?",
            ),
            (
                "-----BEGIN PGP PRIVATE KEY BLOCK-----\n\nlQOYBGZha2UAAAAAlQOYBGZha2UAAAAA\nlQOYBGZha2UAAAAA",
                "[redacted]",
            ),
            (
                "-----BEGIN PRIVATE KEY----- starts a key",
                "[redacted] starts a key",
            ),
            (
                "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----",
                "[redacted]\n-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----",
            ),
            (
                "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-128-CBC,0123456789ABCDEF0123456789ABCDEF\n\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC",
                "[redacted]",
            ),
            (
                "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC  \nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\t\nAAAAAAAA==  \n",
                "[redacted]  \n",
            ),
            (
                r#""-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC  \nAAAAAAAA==""#,
                r#""[redacted]""#,
            ),
        ] {
            assert_eq!(redact(text), redacted, "{text}");
        }
    }

    #[test]
    fn a_key_after_a_json_strings_escape_is_redacted() {
        let mut secrets: Vec<(String, &str)> = PREFIXES
            .iter()
            .map(|(prefix, least)| (format!("{prefix}{}", "A".repeat(*least)), "[redacted]"))
            .collect();
        secrets.push((
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.abcdefghijklmnop".into(),
            "[redacted]",
        ));
        secrets.push((
            "Bearer abcdefghijklmnopqrstuvwxyz012345".into(),
            "Bearer [redacted]",
        ));
        for (secret, redacted) in &secrets {
            for escape in [r"\n", r"\t", r"\r"] {
                assert_eq!(
                    redact(&format!(r#"{{"content": "line one{escape}{secret}"}}"#)),
                    format!(r#"{{"content": "line one{escape}{redacted}"}}"#)
                );
            }
        }
    }
}
