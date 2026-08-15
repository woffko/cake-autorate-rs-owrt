//! Small, allocation-bounded JSON primitives for private control responses.
//!
//! The coordinator deliberately does not depend on a general-purpose JSON
//! serializer for its root-owned private wire contracts.  Keep the escaping
//! rule in one place so scheduler status and operation responses cannot drift.

pub(crate) fn bool_json(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

pub(crate) fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            character if character.is_control() => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", character as u32);
            }
            character => out.push(character),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_json_primitives_are_exact() {
        assert_eq!(bool_json(true), "true");
        assert_eq!(bool_json(false), "false");
        assert_eq!(
            json_escape("quote=\" slash=\\ newline=\n tab=\t control=\u{0007}"),
            "quote=\\\" slash=\\\\ newline=\\n tab=\\t control=\\u0007"
        );
    }
}
