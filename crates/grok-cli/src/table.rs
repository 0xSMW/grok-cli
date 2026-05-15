use crate::terminal::visible_length;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TableAlignment {
    Left,
    Right,
    Center,
}

pub fn parse_table_row(line: &str) -> Option<Vec<String>> {
    let mut trimmed = line.trim();
    if !trimmed.contains('|') {
        return None;
    }

    if let Some(rest) = trimmed.strip_prefix('|') {
        trimmed = rest;
    }
    if let Some(rest) = trimmed.strip_suffix('|') {
        trimmed = rest;
    }

    if trimmed.is_empty() {
        return Some(Vec::new());
    }

    Some(
        trimmed
            .split('|')
            .map(|cell| cell.trim().to_string())
            .collect(),
    )
}

pub fn parse_table_separator(line: &str) -> Option<Vec<TableAlignment>> {
    let cells = parse_table_row(line)?;
    if cells.is_empty() {
        return None;
    }

    let mut alignments = Vec::new();
    for cell in cells {
        let normalized = cell.replace(' ', "");
        let dash_count = normalized
            .chars()
            .filter(|character| *character == '-')
            .count();
        if dash_count < 1
            || !normalized
                .chars()
                .all(|character| character == '-' || character == ':')
        {
            return None;
        }

        if normalized.starts_with(':') && normalized.ends_with(':') {
            alignments.push(TableAlignment::Center);
        } else if normalized.ends_with(':') {
            alignments.push(TableAlignment::Right);
        } else {
            alignments.push(TableAlignment::Left);
        }
    }

    Some(alignments)
}

pub fn normalized_table_row<T: Clone>(row: &[T], count: usize, default_value: T) -> Vec<T> {
    if row.len() >= count {
        return row.iter().take(count).cloned().collect();
    }

    let mut normalized = row.to_vec();
    normalized.extend(std::iter::repeat_n(default_value, count - row.len()));
    normalized
}

pub fn normalized_string_table_row(row: &[String], count: usize) -> Vec<String> {
    normalized_table_row(row, count, String::new())
}

pub fn table_column_widths(rows: &[Vec<String>], column_count: usize) -> Vec<usize> {
    (0..column_count)
        .map(|column| {
            rows.iter()
                .filter_map(|row| row.get(column))
                .map(|cell| visible_length(cell))
                .max()
                .unwrap_or(0)
        })
        .collect()
}

pub fn render_table_row(row: &[String], widths: &[usize], alignments: &[TableAlignment]) -> String {
    let cells = row
        .iter()
        .enumerate()
        .map(|(index, cell)| {
            format!(
                " {} ",
                padded(
                    cell,
                    widths.get(index).copied().unwrap_or_default(),
                    alignments
                        .get(index)
                        .copied()
                        .unwrap_or(TableAlignment::Left)
                )
            )
        })
        .collect::<Vec<_>>();
    format!("|{}|", cells.join("|"))
}

pub fn render_table_separator(widths: &[usize], alignments: &[TableAlignment]) -> String {
    let cells = widths
        .iter()
        .enumerate()
        .map(|(index, width)| {
            let dashes = "-".repeat((*width).max(3) + 2);
            match alignments
                .get(index)
                .copied()
                .unwrap_or(TableAlignment::Left)
            {
                TableAlignment::Left => dashes,
                TableAlignment::Right => format!("{}:", &dashes[..dashes.len() - 1]),
                TableAlignment::Center => {
                    format!(":{}:", &dashes[1..dashes.len() - 1])
                }
            }
        })
        .collect::<Vec<_>>();
    format!("|{}|", cells.join("|"))
}

pub fn padded(value: &str, width: usize, alignment: TableAlignment) -> String {
    let missing = width.saturating_sub(visible_length(value));
    match alignment {
        TableAlignment::Left => format!("{value}{}", " ".repeat(missing)),
        TableAlignment::Right => format!("{}{value}", " ".repeat(missing)),
        TableAlignment::Center => {
            let left = missing / 2;
            let right = missing - left;
            format!("{}{value}{}", " ".repeat(left), " ".repeat(right))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        TableAlignment, normalized_string_table_row, normalized_table_row, padded, parse_table_row,
        parse_table_separator, render_table_row, render_table_separator, table_column_widths,
    };

    #[test]
    fn parse_table_row_trims_cells_and_preserves_empty_cells_like_swift() {
        assert_eq!(
            parse_table_row(" | Name |  | Status | "),
            Some(vec![
                "Name".to_string(),
                String::new(),
                "Status".to_string()
            ])
        );
        assert_eq!(
            parse_table_row("Name | Value"),
            Some(vec!["Name".to_string(), "Value".to_string()])
        );
        assert_eq!(parse_table_row("not a table"), None);
    }

    #[test]
    fn parse_table_separator_detects_swift_alignment_markers() {
        assert_eq!(
            parse_table_separator("| --- | ---: | :---: |"),
            Some(vec![
                TableAlignment::Left,
                TableAlignment::Right,
                TableAlignment::Center
            ])
        );
        assert_eq!(parse_table_separator("| : | --- |"), None);
        assert_eq!(parse_table_separator("| --- | nope |"), None);
    }

    #[test]
    fn normalized_table_row_pads_or_truncates_like_swift() {
        assert_eq!(normalized_table_row(&[1, 2, 3], 2, 0), vec![1, 2]);
        assert_eq!(normalized_table_row(&[1, 2], 4, 0), vec![1, 2, 0, 0]);
        assert_eq!(
            normalized_string_table_row(&["a".to_string()], 3),
            vec!["a".to_string(), String::new(), String::new()]
        );
    }

    #[test]
    fn table_column_widths_use_visible_lengths_like_swift() {
        let rows = vec![
            vec![
                "\u{001b}[33mName\u{001b}[0m".to_string(),
                "Value".to_string(),
            ],
            vec!["Longer".to_string(), "東京".to_string()],
        ];

        assert_eq!(table_column_widths(&rows, 2), vec![6, 5]);
    }

    #[test]
    fn render_table_row_aligns_cells_like_swift() {
        let row = vec!["Name".to_string(), "42".to_string(), "OK".to_string()];

        assert_eq!(
            render_table_row(
                &row,
                &[5, 4, 6],
                &[
                    TableAlignment::Left,
                    TableAlignment::Right,
                    TableAlignment::Center
                ]
            ),
            "| Name  |   42 |   OK   |"
        );
        assert_eq!(padded("OK", 5, TableAlignment::Center), " OK  ");
    }

    #[test]
    fn render_table_separator_matches_swift_markdown_shape() {
        assert_eq!(
            render_table_separator(
                &[2, 4, 5],
                &[
                    TableAlignment::Left,
                    TableAlignment::Right,
                    TableAlignment::Center
                ]
            ),
            "|-----|-----:|:-----:|"
        );
    }
}
