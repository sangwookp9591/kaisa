//! Minimal JSON text helpers for event payloads written by the ledger itself.

/// `text` as a JSON string literal.
///
/// Quotes, backslashes and control characters are escaped; every other character, including
/// non-ASCII text, is kept as is because it is valid JSON.
#[must_use]
pub(crate) fn json_string(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len());
    quoted.push('"');
    for character in text.chars() {
        match character {
            '"' => quoted.push_str(r#"\""#),
            '\\' => quoted.push_str(r"\\"),
            '\n' => quoted.push_str(r"\n"),
            '\r' => quoted.push_str(r"\r"),
            '\t' => quoted.push_str(r"\t"),
            control if control.is_control() => {
                quoted.push_str(&format!(r"\u{:04x}", u32::from(control)));
            }
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::json_string;

    #[test]
    fn quotes_and_backslashes_in_a_windows_path_are_escaped() {
        let windows_receipt: &str = r#"created "C:\kaisa\att_41a""#;

        assert_eq!(
            json_string(windows_receipt),
            r#""created \"C:\\kaisa\\att_41a\"""#,
            "quotes and backslashes must not end or corrupt the JSON string"
        );
    }

    #[test]
    fn korean_text_is_kept_as_readable_text() {
        let korean_receipt: &str = "작업 트리 생성 완료";

        assert_eq!(
            json_string(korean_receipt),
            format!("\"{korean_receipt}\""),
            "non-ASCII text is valid JSON as is and must stay readable"
        );
    }

    #[test]
    fn a_line_break_is_escaped_so_the_string_stays_on_one_line() {
        let two_line_receipt: &str = "created\nverified";

        assert_eq!(
            json_string(two_line_receipt),
            r#""created\nverified""#,
            "a raw newline inside a JSON string is invalid"
        );
    }

    #[test]
    fn a_bell_character_is_escaped_as_a_unicode_escape() {
        let receipt_with_bell: &str = "done\u{7}";

        assert_eq!(
            json_string(receipt_with_bell),
            r#""done\u0007""#,
            "control characters must be written as \\u escapes"
        );
    }
}
