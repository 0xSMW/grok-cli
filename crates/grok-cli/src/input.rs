use crate::InteractiveCompletionSuggestion;
use crate::terminal::{truncate_end, visible_length};
use anyhow::Result;
use crossterm::{
    cursor,
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind,
        KeyModifiers,
    },
    execute, queue,
    terminal::{self, ClearType},
};
use std::io::{IsTerminal, Write};

#[derive(Clone, Debug, Eq, PartialEq)]
enum InputSegment {
    Text(String),
    Pasted { display: String, content: String },
}

impl InputSegment {
    fn display_text(&self) -> &str {
        match self {
            Self::Text(text) => text,
            Self::Pasted { display, .. } => display,
        }
    }

    fn actual_text(&self) -> &str {
        match self {
            Self::Text(text) => text,
            Self::Pasted { content, .. } => content,
        }
    }

    fn display_count(&self) -> usize {
        self.display_text().chars().count()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InputLineBuffer {
    segments: Vec<InputSegment>,
}

impl InputLineBuffer {
    pub fn new(text: &str) -> Self {
        if text.is_empty() {
            Self::default()
        } else {
            Self {
                segments: vec![InputSegment::Text(text.to_string())],
            }
        }
    }

    pub fn display(&self) -> String {
        self.segments
            .iter()
            .map(InputSegment::display_text)
            .collect::<String>()
    }

    pub fn rendered_display(&self) -> String {
        self.display()
    }

    pub fn actual(&self) -> String {
        self.segments
            .iter()
            .map(InputSegment::actual_text)
            .collect::<String>()
    }

    pub fn display_count(&self) -> usize {
        self.segments.iter().map(InputSegment::display_count).sum()
    }

    pub fn clear(&mut self) {
        self.segments.clear();
    }

    pub fn replace(&mut self, text: &str) {
        self.segments = if text.is_empty() {
            Vec::new()
        } else {
            vec![InputSegment::Text(text.to_string())]
        };
    }

    pub fn replace_with_committed_text(&mut self, text: &str) {
        if Self::should_collapse_pasted_content(text) {
            self.segments = vec![InputSegment::Pasted {
                display: Self::paste_placeholder(text.chars().count()),
                content: text.to_string(),
            }];
        } else {
            self.replace(text);
        }
    }

    pub fn insert_character(&mut self, character: char, cursor_index: usize) -> usize {
        self.insert_segment(InputSegment::Text(character.to_string()), cursor_index)
    }

    pub fn insert_pasted_content(&mut self, content: &str, cursor_index: usize) -> usize {
        if content.is_empty() {
            return self.clamped_cursor(cursor_index);
        }

        if Self::should_collapse_pasted_content(content) {
            return self.insert_segment(
                InputSegment::Pasted {
                    display: Self::paste_placeholder(content.chars().count()),
                    content: content.to_string(),
                },
                cursor_index,
            );
        }

        self.insert_text(content, cursor_index)
    }

    pub fn backspace(&mut self, cursor_index: usize) -> usize {
        let clamped = self.clamped_cursor(cursor_index);
        if clamped == 0 {
            return 0;
        }
        self.remove_display_character(clamped - 1, clamped - 1)
    }

    pub fn delete_forward(&mut self, cursor_index: usize) -> usize {
        let clamped = self.clamped_cursor(cursor_index);
        if clamped >= self.display_count() {
            return clamped;
        }
        self.remove_display_character(clamped, clamped)
    }

    pub fn should_collapse_pasted_content(content: &str) -> bool {
        content.chars().count() >= 128 || content.contains('\n') || content.contains('\r')
    }

    pub fn paste_placeholder(character_count: usize) -> String {
        let unit = if character_count == 1 {
            "char"
        } else {
            "chars"
        };
        format!("[Pasted content {character_count} {unit}]")
    }

    fn insert_text(&mut self, text: &str, cursor_index: usize) -> usize {
        let mut cursor = self.clamped_cursor(cursor_index);
        for character in text.chars() {
            cursor = self.insert_character(character, cursor);
        }
        cursor
    }

    fn insert_segment(&mut self, segment: InputSegment, cursor_index: usize) -> usize {
        let insertion = self.insertion_point(cursor_index);
        let display_count = segment.display_count();
        self.segments.insert(insertion.segment_index, segment);
        self.normalize_segments();
        insertion.cursor_index + display_count
    }

    fn insertion_point(&mut self, cursor_index: usize) -> InsertionPoint {
        let clamped = self.clamped_cursor(cursor_index);
        let mut offset = 0;

        for index in 0..self.segments.len() {
            let length = self.segments[index].display_count();
            let start = offset;
            let end = offset + length;

            if clamped <= start {
                return InsertionPoint {
                    segment_index: index,
                    cursor_index: clamped,
                };
            }

            if clamped < end {
                match self.segments[index].clone() {
                    InputSegment::Text(text) => {
                        let split_offset = clamped - start;
                        let before = take_chars(&text, 0, split_offset);
                        let after = take_chars(&text, split_offset, text.chars().count());
                        let mut replacement = Vec::new();
                        if !before.is_empty() {
                            replacement.push(InputSegment::Text(before));
                        }
                        if !after.is_empty() {
                            replacement.push(InputSegment::Text(after));
                        }
                        self.segments.splice(index..=index, replacement);
                        return InsertionPoint {
                            segment_index: index + usize::from(split_offset > 0),
                            cursor_index: clamped,
                        };
                    }
                    InputSegment::Pasted { .. } => {
                        self.segments.remove(index);
                        return InsertionPoint {
                            segment_index: index,
                            cursor_index: start,
                        };
                    }
                }
            }

            if clamped == end {
                return InsertionPoint {
                    segment_index: index + 1,
                    cursor_index: clamped,
                };
            }

            offset = end;
        }

        InsertionPoint {
            segment_index: self.segments.len(),
            cursor_index: clamped,
        }
    }

    fn remove_display_character(&mut self, display_index: usize, fallback_cursor: usize) -> usize {
        let mut offset = 0;

        for index in 0..self.segments.len() {
            let length = self.segments[index].display_count();
            let start = offset;
            let end = offset + length;

            if display_index >= start && display_index < end {
                match self.segments[index].clone() {
                    InputSegment::Text(text) => {
                        let removal_offset = display_index - start;
                        let updated = remove_char_at(&text, removal_offset);
                        if updated.is_empty() {
                            self.segments.remove(index);
                        } else {
                            self.segments[index] = InputSegment::Text(updated);
                        }
                        self.normalize_segments();
                        return fallback_cursor;
                    }
                    InputSegment::Pasted { .. } => {
                        self.segments.remove(index);
                        self.normalize_segments();
                        return start;
                    }
                }
            }

            offset = end;
        }

        self.clamped_cursor(fallback_cursor)
    }

    fn clamped_cursor(&self, cursor_index: usize) -> usize {
        cursor_index.min(self.display_count())
    }

    fn normalize_segments(&mut self) {
        let mut normalized = Vec::new();
        for segment in self.segments.drain(..) {
            match segment {
                InputSegment::Text(text) if text.is_empty() => {}
                InputSegment::Text(text) => {
                    if let Some(InputSegment::Text(previous_text)) = normalized.last_mut() {
                        previous_text.push_str(&text);
                    } else {
                        normalized.push(InputSegment::Text(text));
                    }
                }
                InputSegment::Pasted { .. } => normalized.push(segment),
            }
        }
        self.segments = normalized;
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InputCompletionState {
    selected_suggestion_index: Option<usize>,
}

impl InputCompletionState {
    pub fn selected_suggestion_index(&self) -> Option<usize> {
        self.selected_suggestion_index
    }

    pub fn clear_selection(&mut self) {
        self.selected_suggestion_index = None;
    }

    pub fn apply_completion(
        &mut self,
        buffer: &mut InputLineBuffer,
        suggestions: &[InteractiveCompletionSuggestion],
    ) -> usize {
        if suggestions.is_empty() {
            return buffer.display_count();
        }

        if let Some(suggestion) = self.selected_suggestion(suggestions) {
            return self.apply_suggestion(buffer, suggestion, true);
        }

        if suggestions.len() == 1 {
            return self.apply_suggestion(buffer, &suggestions[0], true);
        }

        let display = buffer.display();
        let shared_prefix = longest_common_prefix(
            suggestions
                .iter()
                .map(|suggestion| suggestion.insert_text.as_str()),
        );
        if shared_prefix.chars().count() > display.chars().count() {
            buffer.replace(&shared_prefix);
            return buffer.display_count();
        }

        buffer.display_count()
    }

    pub fn move_suggestion_selection(
        &mut self,
        delta: isize,
        suggestions: &[InteractiveCompletionSuggestion],
    ) -> bool {
        if suggestions.is_empty() {
            self.selected_suggestion_index = None;
            return false;
        }

        let current = self.selected_suggestion_index.map_or_else(
            || {
                if delta > 0 {
                    -1
                } else {
                    suggestions.len() as isize
                }
            },
            |index| index as isize,
        );
        let next = (current + delta).rem_euclid(suggestions.len() as isize) as usize;
        self.selected_suggestion_index = Some(next);
        true
    }

    pub fn accept_selected_suggestion(
        &mut self,
        buffer: &mut InputLineBuffer,
        suggestions: &[InteractiveCompletionSuggestion],
    ) -> Option<InputCompletionAcceptance> {
        let suggestion = self.selected_suggestion(suggestions)?;
        let should_submit = !suggestion.requires_argument;
        let cursor_index = self.apply_suggestion(buffer, suggestion, !should_submit);
        Some(InputCompletionAcceptance {
            cursor_index,
            should_submit,
        })
    }

    fn selected_suggestion<'a>(
        &self,
        suggestions: &'a [InteractiveCompletionSuggestion],
    ) -> Option<&'a InteractiveCompletionSuggestion> {
        self.selected_suggestion_index
            .and_then(|index| suggestions.get(index))
    }

    fn apply_suggestion(
        &mut self,
        buffer: &mut InputLineBuffer,
        suggestion: &InteractiveCompletionSuggestion,
        appending_trailing_space: bool,
    ) -> usize {
        let mut text = suggestion.insert_text.clone();
        if appending_trailing_space && suggestion.appends_trailing_space && !text.ends_with(' ') {
            text.push(' ');
        }
        buffer.replace(&text);
        self.selected_suggestion_index = None;
        buffer.display_count()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputCompletionAcceptance {
    pub cursor_index: usize,
    pub should_submit: bool,
}

#[derive(Debug, Eq, PartialEq)]
pub enum TerminalInputResult {
    Submitted(String),
    Cancelled,
    Unavailable,
}

pub fn read_terminal_line<F>(
    prompt: &str,
    prefill: &str,
    mut completion_provider: F,
) -> Result<TerminalInputResult>
where
    F: FnMut(&str) -> Vec<InteractiveCompletionSuggestion>,
{
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Ok(TerminalInputResult::Unavailable);
    }

    let mut stdout = std::io::stdout();
    let _guard = match RawTerminalGuard::activate(&mut stdout) {
        Ok(guard) => guard,
        Err(_) => return Ok(TerminalInputResult::Unavailable),
    };

    let mut buffer = InputLineBuffer::new(prefill);
    let mut cursor_index = buffer.display_count();
    let mut completion_state = InputCompletionState::default();
    let mut suggestions = completion_provider(&buffer.display());
    let mut previous_rows = 0;

    loop {
        previous_rows = render_input_editor(
            &mut stdout,
            previous_rows,
            prompt,
            &buffer,
            cursor_index,
            &suggestions,
            completion_state.selected_suggestion_index(),
        )?;

        let event = event::read()?;
        let Event::Key(key) = event else {
            if let Event::Paste(content) = event {
                cursor_index = buffer.insert_pasted_content(&content, cursor_index);
                completion_state.clear_selection();
                suggestions = completion_provider(&buffer.display());
            }
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }

        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                clear_input_editor(&mut stdout, previous_rows)?;
                writeln!(stdout)?;
                stdout.flush()?;
                return Ok(TerminalInputResult::Cancelled);
            }
            KeyCode::Char('d')
                if key.modifiers.contains(KeyModifiers::CONTROL) && buffer.display_count() == 0 =>
            {
                clear_input_editor(&mut stdout, previous_rows)?;
                writeln!(stdout)?;
                stdout.flush()?;
                return Ok(TerminalInputResult::Cancelled);
            }
            KeyCode::Char('j' | 'm') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(result) = submit_input_line(
                    SubmitInputContext {
                        stdout: &mut stdout,
                        previous_rows,
                        prompt,
                        buffer: &mut buffer,
                        cursor_index: &mut cursor_index,
                        completion_state: &mut completion_state,
                        suggestions: &mut suggestions,
                    },
                    &mut completion_provider,
                )? {
                    return Ok(result);
                }
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                buffer.clear();
                cursor_index = 0;
                completion_state.clear_selection();
                suggestions = completion_provider(&buffer.display());
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                cursor_index = 0;
                completion_state.clear_selection();
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                cursor_index = buffer.display_count();
                completion_state.clear_selection();
            }
            KeyCode::Enter => {
                if let Some(result) = submit_input_line(
                    SubmitInputContext {
                        stdout: &mut stdout,
                        previous_rows,
                        prompt,
                        buffer: &mut buffer,
                        cursor_index: &mut cursor_index,
                        completion_state: &mut completion_state,
                        suggestions: &mut suggestions,
                    },
                    &mut completion_provider,
                )? {
                    return Ok(result);
                }
            }
            KeyCode::Tab => {
                cursor_index = completion_state.apply_completion(&mut buffer, &suggestions);
                suggestions = completion_provider(&buffer.display());
            }
            KeyCode::BackTab if completion_state.move_suggestion_selection(-1, &suggestions) => {
                continue;
            }
            KeyCode::Up => {
                let _ = completion_state.move_suggestion_selection(-1, &suggestions);
            }
            KeyCode::Down => {
                let _ = completion_state.move_suggestion_selection(1, &suggestions);
            }
            KeyCode::Left => {
                cursor_index = cursor_index.saturating_sub(1);
                completion_state.clear_selection();
            }
            KeyCode::Right => {
                cursor_index = (cursor_index + 1).min(buffer.display_count());
                completion_state.clear_selection();
            }
            KeyCode::Home => {
                cursor_index = 0;
                completion_state.clear_selection();
            }
            KeyCode::End => {
                cursor_index = buffer.display_count();
                completion_state.clear_selection();
            }
            KeyCode::Backspace => {
                cursor_index = buffer.backspace(cursor_index);
                completion_state.clear_selection();
                suggestions = completion_provider(&buffer.display());
            }
            KeyCode::Delete => {
                cursor_index = buffer.delete_forward(cursor_index);
                completion_state.clear_selection();
                suggestions = completion_provider(&buffer.display());
            }
            KeyCode::Esc => {
                completion_state.clear_selection();
            }
            KeyCode::Char(_) if key.modifiers.contains(KeyModifiers::CONTROL) => {}
            KeyCode::Char(character) => {
                cursor_index = buffer.insert_character(character, cursor_index);
                completion_state.clear_selection();
                suggestions = completion_provider(&buffer.display());
            }
            _ => {}
        }
    }
}

struct SubmitInputContext<'a> {
    stdout: &'a mut std::io::Stdout,
    previous_rows: usize,
    prompt: &'a str,
    buffer: &'a mut InputLineBuffer,
    cursor_index: &'a mut usize,
    completion_state: &'a mut InputCompletionState,
    suggestions: &'a mut Vec<InteractiveCompletionSuggestion>,
}

fn submit_input_line<F>(
    context: SubmitInputContext<'_>,
    completion_provider: &mut F,
) -> std::io::Result<Option<TerminalInputResult>>
where
    F: FnMut(&str) -> Vec<InteractiveCompletionSuggestion>,
{
    if let Some(acceptance) = context
        .completion_state
        .accept_selected_suggestion(context.buffer, context.suggestions)
    {
        *context.cursor_index = acceptance.cursor_index;
        *context.suggestions = completion_provider(&context.buffer.display());
        if !acceptance.should_submit {
            return Ok(None);
        }
    }
    clear_input_editor(context.stdout, context.previous_rows)?;
    writeln!(
        context.stdout,
        "{}{}",
        context.prompt,
        context.buffer.display()
    )?;
    context.stdout.flush()?;
    Ok(Some(TerminalInputResult::Submitted(
        context.buffer.actual(),
    )))
}

struct RawTerminalGuard;

impl RawTerminalGuard {
    fn activate(stdout: &mut std::io::Stdout) -> std::io::Result<Self> {
        terminal::enable_raw_mode()?;
        execute!(stdout, EnableBracketedPaste)?;
        Ok(Self)
    }
}

impl Drop for RawTerminalGuard {
    fn drop(&mut self) {
        let mut stdout = std::io::stdout();
        let _ = execute!(stdout, DisableBracketedPaste);
        let _ = terminal::disable_raw_mode();
    }
}

fn render_input_editor(
    stdout: &mut std::io::Stdout,
    previous_rows: usize,
    prompt: &str,
    buffer: &InputLineBuffer,
    cursor_index: usize,
    suggestions: &[InteractiveCompletionSuggestion],
    selected_suggestion_index: Option<usize>,
) -> std::io::Result<usize> {
    clear_input_editor(stdout, previous_rows)?;

    let width = terminal_width();
    let suggestion_lines = suggestion_lines(suggestions, selected_suggestion_index, width);
    for line in &suggestion_lines {
        writeln!(stdout, "{line}")?;
    }

    let display = buffer.rendered_display();
    let ghost = ghost_suffix(&display, suggestions, selected_suggestion_index);
    write!(stdout, "{prompt}{display}{}", dimmed(&ghost))?;

    let end_column = visible_length(prompt) + visible_length(&display) + visible_length(&ghost);
    let target_column = visible_length(prompt) + cursor_index.min(buffer.display_count());
    if end_column > target_column {
        queue!(
            stdout,
            cursor::MoveLeft((end_column - target_column).min(u16::MAX as usize) as u16)
        )?;
    }
    stdout.flush()?;

    Ok(suggestion_lines.len()
        + wrapped_line_count(visible_length(prompt) + buffer.display_count(), width))
}

fn clear_input_editor(stdout: &mut std::io::Stdout, previous_rows: usize) -> std::io::Result<()> {
    if previous_rows == 0 {
        return Ok(());
    }

    queue!(
        stdout,
        cursor::MoveToColumn(0),
        terminal::Clear(ClearType::CurrentLine)
    )?;
    for _ in 1..previous_rows {
        queue!(
            stdout,
            cursor::MoveUp(1),
            cursor::MoveToColumn(0),
            terminal::Clear(ClearType::CurrentLine)
        )?;
    }
    stdout.flush()
}

fn suggestion_lines(
    suggestions: &[InteractiveCompletionSuggestion],
    selected_suggestion_index: Option<usize>,
    width: usize,
) -> Vec<String> {
    if suggestions.is_empty() {
        return Vec::new();
    }

    const VISIBLE_SUGGESTION_ROWS: usize = 3;
    let start = suggestion_window_start(
        suggestions.len(),
        selected_suggestion_index,
        VISIBLE_SUGGESTION_ROWS,
    );
    let end = (start + VISIBLE_SUGGESTION_ROWS).min(suggestions.len());
    let command_width = suggestions[start..end]
        .iter()
        .map(|suggestion| visible_length(&suggestion.display))
        .max()
        .unwrap_or(0)
        .clamp(8, 28);

    let mut lines = Vec::new();
    for (index, suggestion) in suggestions[start..end].iter().enumerate() {
        let absolute_index = start + index;
        let marker = if Some(absolute_index) == selected_suggestion_index {
            "> "
        } else {
            "  "
        };
        let display = pad_end(
            &truncate_end(&suggestion.display, command_width),
            command_width,
        );
        let description = if suggestion.description.is_empty() {
            String::new()
        } else {
            format!(" {}", suggestion.description)
        };
        let line = format!(
            "{marker}{}{}",
            yellow(&display, Some(absolute_index) == selected_suggestion_index),
            description
        );
        lines.push(truncate_end(&line, width));
    }
    lines.push(truncate_end(
        &blue("tab complete  arrows select  enter run"),
        width,
    ));
    lines
}

fn terminal_width() -> usize {
    terminal::size()
        .map(|(columns, _)| usize::from(columns).max(1))
        .unwrap_or(120)
}

fn pad_end(value: &str, width: usize) -> String {
    let value_length = visible_length(value);
    if value_length >= width {
        value.to_string()
    } else {
        format!("{value}{}", " ".repeat(width - value_length))
    }
}

fn yellow(value: &str, bold: bool) -> String {
    if bold {
        format!("\u{001b}[1;33m{value}\u{001b}[0m")
    } else {
        format!("\u{001b}[33m{value}\u{001b}[0m")
    }
}

fn blue(value: &str) -> String {
    format!("\u{001b}[34m{value}\u{001b}[0m")
}

fn dimmed(value: &str) -> String {
    if value.is_empty() {
        String::new()
    } else {
        format!("\u{001b}[90m{value}\u{001b}[0m")
    }
}

pub fn suggestion_window_start(
    suggestion_count: usize,
    selected_index: Option<usize>,
    visible_suggestion_rows: usize,
) -> usize {
    if suggestion_count <= visible_suggestion_rows || visible_suggestion_rows == 0 {
        return 0;
    }

    let Some(selected_index) = selected_index else {
        return 0;
    };
    let clamped_selection = selected_index.min(suggestion_count.saturating_sub(1));
    if clamped_selection < visible_suggestion_rows {
        return 0;
    }
    (clamped_selection - visible_suggestion_rows + 1)
        .min(suggestion_count - visible_suggestion_rows)
}

pub fn ghost_suffix(
    buffer: &str,
    suggestions: &[InteractiveCompletionSuggestion],
    selected_index: Option<usize>,
) -> String {
    if buffer.is_empty() {
        return String::new();
    }

    let suggestion = selected_index
        .and_then(|index| suggestions.get(index))
        .or_else(|| suggestions.first());
    let Some(suggestion) = suggestion else {
        return String::new();
    };

    let insert_text = &suggestion.insert_text;
    if insert_text.chars().count() <= buffer.chars().count()
        || !insert_text
            .to_lowercase()
            .starts_with(&buffer.to_lowercase())
    {
        return String::new();
    }
    insert_text.chars().skip(buffer.chars().count()).collect()
}

pub fn longest_common_prefix<'a>(values: impl IntoIterator<Item = &'a str>) -> String {
    let mut values = values.into_iter();
    let Some(mut prefix) = values.next().map(ToOwned::to_owned) else {
        return String::new();
    };

    for value in values {
        while !value.starts_with(&prefix) && !prefix.is_empty() {
            prefix.pop();
        }
    }
    prefix
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InsertionPoint {
    segment_index: usize,
    cursor_index: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WrappedCursorPosition {
    pub row: usize,
    pub column: usize,
}

pub fn wrapped_line_count(visible_length: usize, width: usize) -> usize {
    let position = wrapped_cursor_position(visible_length, visible_length, width);
    (position.row + 1).max(1)
}

pub fn wrapped_cursor_position(
    visible_offset: usize,
    visible_length: usize,
    width: usize,
) -> WrappedCursorPosition {
    let normalized_width = width.max(1);

    let adjusted_offset = if visible_offset == visible_length
        && visible_offset > 0
        && visible_offset.is_multiple_of(normalized_width)
    {
        visible_offset - 1
    } else {
        visible_offset
    };
    WrappedCursorPosition {
        row: adjusted_offset / normalized_width,
        column: adjusted_offset % normalized_width,
    }
}

fn take_chars(text: &str, start: usize, end: usize) -> String {
    text.chars().skip(start).take(end - start).collect()
}

fn remove_char_at(text: &str, offset: usize) -> String {
    text.chars()
        .enumerate()
        .filter_map(|(index, character)| (index != offset).then_some(character))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        InputCompletionAcceptance, InputCompletionState, InputLineBuffer, WrappedCursorPosition,
        ghost_suffix, longest_common_prefix, suggestion_window_start, wrapped_cursor_position,
        wrapped_line_count,
    };
    use crate::InteractiveCompletionSuggestion;

    fn completion(
        display: &str,
        insert_text: &str,
        requires_argument: bool,
        appends_trailing_space: bool,
    ) -> InteractiveCompletionSuggestion {
        InteractiveCompletionSuggestion {
            display: display.to_string(),
            insert_text: insert_text.to_string(),
            description: String::new(),
            requires_argument,
            appends_trailing_space,
        }
    }

    #[test]
    fn input_line_buffer_collapses_large_paste_and_expands_on_submit_like_swift() {
        let pasted_text = "a".repeat(160);
        let mut buffer = InputLineBuffer::new("summarize ");

        let cursor = buffer.insert_pasted_content(&pasted_text, buffer.display_count());

        assert_eq!(buffer.display(), "summarize [Pasted content 160 chars]");
        assert_eq!(buffer.rendered_display(), buffer.display());
        assert_eq!(buffer.actual(), format!("summarize {pasted_text}"));
        assert_eq!(cursor, buffer.display_count());
    }

    #[test]
    fn input_line_buffer_collapses_multiline_paste_and_deletes_atomically_like_swift() {
        let pasted_text = "line one\nline two\nline three";
        let mut buffer = InputLineBuffer::new("review ");
        let cursor_after_paste = buffer.insert_pasted_content(pasted_text, buffer.display_count());

        assert_eq!(buffer.display(), "review [Pasted content 28 chars]");
        assert_eq!(buffer.actual(), format!("review {pasted_text}"));

        let cursor_after_delete = buffer.backspace(cursor_after_paste);

        assert_eq!(cursor_after_delete, "review ".len());
        assert_eq!(buffer.display(), "review ");
        assert_eq!(buffer.actual(), "review ");
    }

    #[test]
    fn input_line_buffer_keeps_small_single_line_paste_inline_like_swift() {
        let mut buffer = InputLineBuffer::new("say ");

        let _ = buffer.insert_pasted_content("hello", buffer.display_count());

        assert_eq!(buffer.display(), "say hello");
        assert_eq!(buffer.actual(), "say hello");
    }

    #[test]
    fn input_completion_applies_selected_single_or_shared_prefix_like_swift() {
        let mut selected_state = InputCompletionState::default();
        let selected_suggestions = vec![
            completion("/model", "/model", false, true),
            completion("/mode", "/mode", false, true),
        ];
        assert!(selected_state.move_suggestion_selection(1, &selected_suggestions));
        assert!(selected_state.move_suggestion_selection(1, &selected_suggestions));
        let mut selected_buffer = InputLineBuffer::new("/m");
        let selected_cursor =
            selected_state.apply_completion(&mut selected_buffer, &selected_suggestions);
        assert_eq!(selected_buffer.display(), "/mode ");
        assert_eq!(selected_cursor, selected_buffer.display_count());
        assert_eq!(selected_state.selected_suggestion_index(), None);

        let mut single_state = InputCompletionState::default();
        let mut single_buffer = InputLineBuffer::new("/sea");
        let single_cursor = single_state.apply_completion(
            &mut single_buffer,
            &[completion("/search", "/search", true, true)],
        );
        assert_eq!(single_buffer.display(), "/search ");
        assert_eq!(single_cursor, single_buffer.display_count());

        let mut prefix_state = InputCompletionState::default();
        let mut prefix_buffer = InputLineBuffer::new("g");
        let prefix_cursor = prefix_state.apply_completion(
            &mut prefix_buffer,
            &[
                completion("grok fast", "grok fast", false, false),
                completion("grok facts", "grok facts", false, false),
            ],
        );
        assert_eq!(prefix_buffer.display(), "grok fa");
        assert_eq!(prefix_cursor, prefix_buffer.display_count());
    }

    #[test]
    fn input_completion_accepts_selected_suggestion_like_swift() {
        let suggestions = vec![
            completion("/help", "/help", false, true),
            completion("/skill create", "/skill create", true, true),
        ];

        let mut submit_state = InputCompletionState::default();
        assert!(submit_state.move_suggestion_selection(1, &suggestions));
        let mut submit_buffer = InputLineBuffer::new("/h");
        assert_eq!(
            submit_state.accept_selected_suggestion(&mut submit_buffer, &suggestions),
            Some(InputCompletionAcceptance {
                cursor_index: "/help".len(),
                should_submit: true,
            })
        );
        assert_eq!(submit_buffer.display(), "/help");
        assert_eq!(submit_state.selected_suggestion_index(), None);

        let mut argument_state = InputCompletionState::default();
        assert!(argument_state.move_suggestion_selection(-1, &suggestions));
        let mut argument_buffer = InputLineBuffer::new("/skill c");
        assert_eq!(
            argument_state.accept_selected_suggestion(&mut argument_buffer, &suggestions),
            Some(InputCompletionAcceptance {
                cursor_index: "/skill create ".len(),
                should_submit: false,
            })
        );
        assert_eq!(argument_buffer.display(), "/skill create ");
        assert_eq!(argument_state.selected_suggestion_index(), None);
    }

    #[test]
    fn input_completion_window_and_ghost_suffix_match_swift() {
        assert_eq!(suggestion_window_start(5, None, 3), 0);
        assert_eq!(suggestion_window_start(5, Some(0), 3), 0);
        assert_eq!(suggestion_window_start(5, Some(3), 3), 1);
        assert_eq!(suggestion_window_start(5, Some(9), 3), 2);

        let suggestions = vec![
            completion(
                "test driven development",
                "test driven development",
                false,
                false,
            ),
            completion("testing rust", "testing rust", false, false),
        ];
        assert_eq!(
            ghost_suffix("test", &suggestions, None),
            " driven development"
        );
        assert_eq!(ghost_suffix("TEST", &suggestions, Some(1)), "ing rust");
        assert_eq!(ghost_suffix("", &suggestions, None), "");
        assert_eq!(ghost_suffix("missing", &suggestions, None), "");
    }

    #[test]
    fn input_completion_longest_common_prefix_matches_swift() {
        assert_eq!(
            longest_common_prefix(["/workspace", "/workspaces"].into_iter()),
            "/workspace"
        );
        assert_eq!(
            longest_common_prefix(["grok fast", "grok facts", "grok fallback"].into_iter()),
            "grok fa"
        );
        assert_eq!(longest_common_prefix(std::iter::empty()), "");
    }

    #[test]
    fn wrapped_prompt_metrics_account_for_long_input_like_swift() {
        assert_eq!(wrapped_line_count(10, 80), 1);
        assert_eq!(wrapped_line_count(80, 80), 1);
        assert_eq!(wrapped_line_count(81, 80), 2);
        assert_eq!(wrapped_line_count(3, 0), 3);
        assert_eq!(
            wrapped_cursor_position(95, 160, 80),
            WrappedCursorPosition { row: 1, column: 15 }
        );
        assert_eq!(
            wrapped_cursor_position(160, 160, 80),
            WrappedCursorPosition { row: 1, column: 79 }
        );
        assert_eq!(
            wrapped_cursor_position(3, 3, 0),
            WrappedCursorPosition { row: 2, column: 0 }
        );
    }
}
