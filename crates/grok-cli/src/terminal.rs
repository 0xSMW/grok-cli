pub fn strip_ansi(value: &str) -> String {
    let mut stripped = String::new();
    let mut chars = value.chars().peekable();

    while let Some(character) = chars.next() {
        if character != '\u{001b}' {
            stripped.push(character);
            continue;
        }

        if chars.peek() != Some(&'[') {
            stripped.push(character);
            continue;
        }

        let mut sequence = String::from(character);
        if let Some(open_bracket) = chars.next() {
            sequence.push(open_bracket);
        }

        let mut is_complete_csi = false;
        while let Some(next) = chars.peek().copied() {
            if ('@'..='~').contains(&next) {
                sequence.push(next);
                let _ = chars.next();
                is_complete_csi = true;
                break;
            }

            if next.is_ascii_digit() || matches!(next, ';' | '?') || (' '..='/').contains(&next) {
                sequence.push(next);
                let _ = chars.next();
                continue;
            }

            break;
        }

        if !is_complete_csi {
            stripped.push_str(&sequence);
        }
    }

    stripped
}

pub fn visible_length(value: &str) -> usize {
    strip_ansi(value).chars().count()
}

pub fn truncate_end(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let plain = strip_ansi(value);
    if plain.chars().count() <= width {
        return value.to_string();
    }

    if width <= 3 {
        return plain.chars().take(width).collect();
    }

    format!("{}...", plain.chars().take(width - 3).collect::<String>())
}

pub fn truncate_middle(value: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let plain = strip_ansi(value);
    let plain_length = plain.chars().count();
    if plain_length <= width {
        return value.to_string();
    }

    if width <= 3 {
        return plain.chars().take(width).collect();
    }

    let head = (width - 3).div_ceil(2);
    let tail = width - 3 - head;
    let prefix = plain.chars().take(head).collect::<String>();
    let suffix = plain
        .chars()
        .skip(plain_length.saturating_sub(tail))
        .collect::<String>();
    format!("{prefix}...{suffix}")
}

#[cfg(test)]
mod tests {
    use super::{strip_ansi, truncate_end, truncate_middle, visible_length};

    #[test]
    fn strip_ansi_removes_swift_terminal_layout_csi_sequences() {
        assert_eq!(
            strip_ansi("\u{001b}[32mGrok\u{001b}[0m > Ready"),
            "Grok > Ready"
        );
        assert_eq!(
            strip_ansi("paste \u{001b}[?2004hmode\u{001b}[?2004l"),
            "paste mode"
        );
        assert_eq!(strip_ansi("move\u{001b}[12;4Hcursor"), "movecursor");
    }

    #[test]
    fn strip_ansi_preserves_incomplete_or_non_csi_escapes_like_swift_regex() {
        assert_eq!(strip_ansi("bad\u{001b}[31"), "bad\u{001b}[31");
        assert_eq!(
            strip_ansi("osc\u{001b}]0;title\u{0007}"),
            "osc\u{001b}]0;title\u{0007}"
        );
    }

    #[test]
    fn visible_length_counts_plain_characters_like_swift_terminal_layout() {
        assert_eq!(visible_length("\u{001b}[1;33mhello\u{001b}[0m"), 5);
        assert_eq!(visible_length("東京"), 2);
    }

    #[test]
    fn truncate_end_matches_swift_terminal_layout_edges() {
        let styled = "\u{001b}[32mshort\u{001b}[0m";
        assert_eq!(truncate_end(styled, 10), styled);
        assert_eq!(truncate_end("abcdefghij", 7), "abcd...");
        assert_eq!(
            truncate_end("\u{001b}[32mabcdefghij\u{001b}[0m", 7),
            "abcd..."
        );
        assert_eq!(truncate_end("abcdefghij", 3), "abc");
        assert_eq!(truncate_end("abcdefghij", 0), "");
    }

    #[test]
    fn truncate_middle_matches_swift_terminal_layout_edges() {
        let styled = "\u{001b}[32mshort\u{001b}[0m";
        assert_eq!(truncate_middle(styled, 10), styled);
        assert_eq!(truncate_middle("abcdefghij", 7), "ab...ij");
        assert_eq!(truncate_middle("abcdefghij", 8), "abc...ij");
        assert_eq!(
            truncate_middle("\u{001b}[32mabcdefghij\u{001b}[0m", 8),
            "abc...ij"
        );
        assert_eq!(truncate_middle("abcdefghij", 2), "ab");
        assert_eq!(truncate_middle("abcdefghij", 0), "");
    }
}
