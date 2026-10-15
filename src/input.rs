//! Input box

use std::ops::Range;

use unicode_width::UnicodeWidthStr;

use crate::cursor::Cursor;

/// Input box with data and a cursor
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Input {
    pub data: String,
    pub cursor: Cursor,
    /// Original partial that started a tab-completion cycle (e.g., "th" for thumbsup).
    /// Set during cycling, cleared on any other input action.
    pub completion_partial: Option<String>,
    /// Names of the people that can be mentioned in this input.
    ///
    /// A mention (`@name`) that matches one of these names in full is moved over and deleted as a
    /// unit.
    pub mention_names: Vec<String>,
}

impl Input {
    pub fn put_char(&mut self, c: char) {
        self.cursor.put(c, &mut self.data);
    }

    pub fn new_line(&mut self) {
        self.cursor.new_line(&mut self.data);
    }

    pub fn on_left(&mut self) {
        self.cursor.move_left(&self.data);
        self.snap_out_of_mention(true);
    }

    pub fn on_right(&mut self) {
        self.cursor.move_right(&self.data);
        self.snap_out_of_mention(false);
    }

    pub fn move_line_down(&mut self) {
        self.cursor.move_line_down(&self.data);
    }

    pub fn move_line_up(&mut self) {
        self.cursor.move_line_up(&self.data);
    }

    pub fn move_back_word(&mut self) {
        self.cursor.move_word_left(&self.data);
        self.snap_out_of_mention(true);
    }

    pub fn move_forward_word(&mut self) {
        self.cursor.move_word_right(&self.data);
        self.snap_out_of_mention(false);
    }

    pub fn on_home(&mut self) {
        self.cursor.start_of_line(&self.data);
    }

    pub fn on_end(&mut self) {
        self.cursor.end_of_line(&self.data);
    }

    pub fn on_backspace(&mut self) {
        if !self.delete_mention_before_cursor() {
            self.cursor.delete_backward(&mut self.data);
        }
    }

    pub fn on_delete(&mut self) {
        let idx = self.cursor.idx;
        let span = self
            .mention_spans()
            .into_iter()
            .find(|span| span.start <= idx && idx < span.end);
        match span {
            Some(span) => self.delete_mention(span),
            None => self.cursor.delete_forward(&mut self.data),
        }
    }

    pub fn on_delete_line(&mut self) {
        self.cursor.delete_line_backward(&mut self.data);
    }

    pub fn on_delete_word(&mut self) {
        if !self.delete_mention_before_cursor() {
            self.cursor.delete_word_backward(&mut self.data);
        }
    }

    pub fn on_delete_suffix(&mut self) {
        self.cursor.delete_suffix(&mut self.data);
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn take(&mut self) -> String {
        self.cursor = Default::default();
        std::mem::take(&mut self.data)
    }

    /// If the character just before the cursor is `:`, look backwards for a
    /// matching opening `:` and check if it's a valid emoji shortcode. If so,
    /// replace `:shortcode:` with the actual emoji and adjust cursor position.
    pub fn convert_emoji_on_colon(&mut self) {
        let idx = self.cursor.idx;
        if idx < 1 || !self.data.is_char_boundary(idx) {
            return;
        }
        if &self.data[idx - 1..idx] != ":" {
            return;
        }

        let before = &self.data[..idx - 1];
        let open_pos = match before.rfind(':') {
            Some(p) => p,
            None => return,
        };

        // Only convert when the opening `:` starts a word, i.e. it is at the beginning of the input
        // or preceded by whitespace. This avoids mangling code such as `xtask::shell::foobar`.
        if let Some(prev) = self.data[..open_pos].chars().next_back()
            && !prev.is_whitespace()
        {
            return;
        }

        let shortcode = &self.data[open_pos + 1..idx - 1];
        let Some(emoji) = emojis::get_by_shortcode(shortcode) else {
            return;
        };
        let emoji_str = emoji.as_str();

        let old_width = self.data[open_pos..idx].width();
        let emoji_width = emoji_str.width();

        self.cursor.col = self.cursor.col.saturating_sub(old_width - emoji_width);
        self.cursor.idx = open_pos + emoji_str.len();
        self.data.replace_range(open_pos..idx, emoji_str);
    }

    /// If the cursor is inside a potential emoji shortcode (`:part`),
    /// complete or cycle to the next matching shortcode on each call.
    /// Returns true if a completion was made.
    pub fn try_complete_emoji(&mut self) -> bool {
        let idx = self.cursor.idx;
        if idx < 1 {
            self.completion_partial = None;
            return false;
        }

        let before = &self.data[..idx];
        let open_pos = match before.rfind(':') {
            Some(p) => p,
            None => {
                self.completion_partial = None;
                return false;
            }
        };

        let current = &self.data[open_pos + 1..idx];
        if current.is_empty() {
            self.completion_partial = None;
            return false;
        }

        // Determine the base partial to match against.
        // If we're cycling (completion_partial is set and current text is
        // related), reuse the original partial. Otherwise start fresh.
        let partial = match &self.completion_partial {
            Some(prev) if current.starts_with(prev.as_str()) || prev.starts_with(current) => {
                prev.as_str()
            }
            _ => {
                self.completion_partial = Some(current.to_string());
                current
            }
        };

        let matches: Vec<&str> = emojis::iter()
            .flat_map(|e| e.shortcodes())
            .filter(|sc| sc.starts_with(partial))
            .collect();

        if matches.is_empty() {
            self.completion_partial = None;
            return false;
        }

        let cur_idx = matches.iter().position(|&sc| sc == current);
        let next_idx = match cur_idx {
            Some(i) => (i + 1) % matches.len(),
            None => 0,
        };

        let next = matches[next_idx];
        let old_width = current.width();
        let new_width = next.width();

        self.data.replace_range(open_pos + 1..idx, next);
        if new_width >= old_width {
            self.cursor.col += new_width - old_width;
        } else {
            self.cursor.col = self.cursor.col.saturating_sub(old_width - new_width);
        }
        self.cursor.idx = open_pos + 1 + next.len();
        true
    }

    /// Returns the byte ranges of the mentions in the input.
    ///
    /// A mention is an `@` at the beginning of the input or after whitespace, followed by one of
    /// the `mention_names` in full, ignoring case. The name must end at a word boundary. Text that
    /// is still the start of a longer name is not a mention yet. This is derived from the text, so
    /// it holds for names that were typed in full as well as for names completed with tab.
    pub fn mention_spans(&self) -> Vec<Range<usize>> {
        let mut spans = Vec::new();
        if self.mention_names.is_empty() {
            return spans;
        }
        let mut pos = 0;
        while let Some(offset) = self.data[pos..].find('@') {
            let at_pos = pos + offset;
            pos = at_pos + 1;
            if self.data[..at_pos]
                .chars()
                .next_back()
                .is_some_and(|prev| !prev.is_whitespace())
            {
                continue;
            }
            if let Some(len) = self.mention_len(&self.data[pos..]) {
                spans.push(at_pos..pos + len);
                pos += len;
            }
        }
        spans
    }

    /// Returns the length in bytes of the longest mentioned name at the start of `text`.
    fn mention_len(&self, text: &str) -> Option<usize> {
        let (name, len) = self
            .mention_names
            .iter()
            .filter_map(|name| Some((name, prefix_len_ignore_case(text, name)?)))
            .filter(|(_, len)| {
                text[*len..]
                    .chars()
                    .next()
                    .is_none_or(|next| !next.is_alphanumeric())
            })
            .max_by_key(|(_, len)| *len)?;

        // At the end of the input a longer name may still be being typed.
        let still_typing = len == text.len()
            && self.mention_names.iter().any(|other| {
                other.chars().count() > name.chars().count()
                    && prefix_len_ignore_case(other, text).is_some()
            });
        (!still_typing).then_some(len)
    }

    /// Deletes the name of the mention that ends at the cursor, or that the cursor is inside.
    fn delete_mention_before_cursor(&mut self) -> bool {
        let idx = self.cursor.idx;
        let span = self
            .mention_spans()
            .into_iter()
            .find(|span| span.start < idx && idx <= span.end);
        match span {
            Some(span) => {
                self.delete_mention(span);
                true
            }
            None => false,
        }
    }

    /// Deletes the name of a mention and keeps its `@`, so that another person can be chosen.
    fn delete_mention(&mut self, span: Range<usize>) {
        let name = span.start + 1..span.end;
        let idx = self.cursor.idx.clamp(name.start, name.end);
        let width_before_cursor = self.data[name.start..idx].width();
        self.data.replace_range(name.clone(), "");
        self.cursor = Cursor::new(
            self.cursor.idx.min(name.start),
            self.cursor.line,
            self.cursor.col.saturating_sub(width_before_cursor),
        );
    }

    /// Moves the cursor out of a mention it has been moved into, to its start or to its end.
    fn snap_out_of_mention(&mut self, towards_start: bool) {
        let idx = self.cursor.idx;
        let Some(span) = self
            .mention_spans()
            .into_iter()
            .find(|span| span.start < idx && idx < span.end)
        else {
            return;
        };
        let (new_idx, col) = if towards_start {
            let width = self.data[span.start..idx].width();
            (span.start, self.cursor.col.saturating_sub(width))
        } else {
            let width = self.data[idx..span.end].width();
            (span.end, self.cursor.col + width)
        };
        self.cursor = Cursor::new(new_idx, self.cursor.line, col);
    }

    /// Returns the byte position of the `@` that starts a mention before the cursor.
    ///
    /// The `@` must be at the beginning of the input or preceded by whitespace, so email addresses
    /// are left alone.
    fn mention_start(&self) -> Option<usize> {
        let before = self.data.get(..self.cursor.idx)?;
        let at_pos = before.rfind('@')?;
        match before[..at_pos].chars().next_back() {
            Some(prev) if !prev.is_whitespace() => None,
            _ => Some(at_pos),
        }
    }

    /// Returns true if the cursor is inside a potential mention (`@part`).
    pub fn in_mention(&self) -> bool {
        self.mention_start().is_some()
    }

    /// If the cursor is inside a potential mention (`@part`), complete or cycle to the next
    /// candidate whose name starts with the partial, ignoring case.
    /// Returns true if a completion was made.
    pub fn try_complete_mention(&mut self, candidates: &[String]) -> bool {
        let idx = self.cursor.idx;
        let Some(at_pos) = self.mention_start() else {
            return false;
        };

        let current = &self.data[at_pos + 1..idx];
        let current_lower = current.to_lowercase();

        // Reuse the original partial while cycling. Otherwise the text after the `@` is the
        // partial. It can contain spaces, because names do, and it simply fails to match anything
        // once the cursor is past the end of a name.
        let partial = match &self.completion_partial {
            Some(prev)
                if current_lower.starts_with(&prev.to_lowercase())
                    || prev.to_lowercase().starts_with(&current_lower) =>
            {
                prev.clone()
            }
            _ => current.to_string(),
        };
        let partial_lower = partial.to_lowercase();

        let matches: Vec<&String> = candidates
            .iter()
            .filter(|name| name.to_lowercase().starts_with(&partial_lower))
            .collect();
        if matches.is_empty() {
            return false;
        }

        let next_idx = match matches
            .iter()
            .position(|name| name.to_lowercase() == current_lower)
        {
            Some(i) => (i + 1) % matches.len(),
            None => 0,
        };
        let next = matches[next_idx].clone();

        let old_width = current.width();
        let new_width = next.width();

        self.data.replace_range(at_pos + 1..idx, &next);
        if new_width >= old_width {
            self.cursor.col += new_width - old_width;
        } else {
            self.cursor.col = self.cursor.col.saturating_sub(old_width - new_width);
        }
        self.cursor.idx = at_pos + 1 + next.len();
        self.completion_partial = Some(partial);
        true
    }
}

/// Returns the length in bytes of the start of `text` that equals `prefix`, ignoring case.
///
/// Returns `None` if `text` does not start with `prefix`.
fn prefix_len_ignore_case(text: &str, prefix: &str) -> Option<usize> {
    let mut chars = text.char_indices();
    for prefix_char in prefix.chars() {
        let (_, text_char) = chars.next()?;
        if !text_char.to_lowercase().eq(prefix_char.to_lowercase()) {
            return None;
        }
    }
    Some(chars.next().map_or(text.len(), |(idx, _)| idx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cursor::Cursor;

    /// Type `s` character by character, mirroring the app's key dispatch: after every `:` we
    /// attempt an emoji conversion.
    fn type_str(input: &mut Input, s: &str) {
        for c in s.chars() {
            input.put_char(c);
            if c == ':' {
                input.convert_emoji_on_colon();
            }
        }
    }

    #[test]
    fn test_convert_emoji_basic() {
        let mut input = Input::default();
        type_str(&mut input, ":dog:");
        assert_eq!(input.data, "🐶");
        // idx = byte length of the emoji, col = display width, still on line 0
        assert_eq!(input.cursor, Cursor::new("🐶".len(), 0, 2));
    }

    #[test]
    fn test_convert_emoji_unknown_shortcode() {
        let mut input = Input::default();
        type_str(&mut input, ":nope:");
        assert_eq!(input.data, ":nope:");
        assert_eq!(input.cursor, Cursor::new(6, 0, 6));
    }

    #[test]
    fn test_convert_emoji_double_colon() {
        let mut input = Input::default();
        type_str(&mut input, "::");
        assert_eq!(input.data, "::");
        assert_eq!(input.cursor, Cursor::new(2, 0, 2));
    }

    #[test]
    fn test_convert_emoji_no_opening_colon() {
        let mut input = Input::default();
        type_str(&mut input, "dog:");
        assert_eq!(input.data, "dog:");
        assert_eq!(input.cursor, Cursor::new(4, 0, 4));
    }

    #[test]
    fn test_convert_emoji_with_leading_text() {
        let mut input = Input::default();
        type_str(&mut input, "hi :dog:");
        assert_eq!(input.data, "hi 🐶");
        assert_eq!(input.cursor, Cursor::new("hi 🐶".len(), 0, 5));
    }

    #[test]
    fn test_convert_emoji_requires_word_boundary() {
        // Opening `:` glued to a preceding word must not trigger conversion,
        // so paths like `xtask::shell::foobar` stay intact.
        let mut input = Input::default();
        type_str(&mut input, "a:dog:");
        assert_eq!(input.data, "a:dog:");
        assert_eq!(input.cursor, Cursor::new(6, 0, 6));

        let mut input = Input::default();
        type_str(&mut input, "xtask::shell::foobar");
        assert_eq!(input.data, "xtask::shell::foobar");
    }

    #[test]
    fn test_convert_emoji_after_newline() {
        // The start of a line counts as a word boundary.
        let mut input = Input::default();
        type_str(&mut input, "foo\n:dog:");
        assert_eq!(input.data, "foo\n🐶");
        assert_eq!(input.cursor, Cursor::new("foo\n🐶".len(), 1, 2));
    }

    #[test]
    fn test_convert_emoji_consecutive_needs_whitespace() {
        // Whitespace-separated shortcodes both convert.
        let mut input = Input::default();
        type_str(&mut input, ":dog: :cat:");
        assert_eq!(input.data, "🐶 🐱");
        assert_eq!(input.cursor, Cursor::new("🐶 🐱".len(), 0, 5));

        // Directly glued to the preceding emoji, the second one is not at a
        // word boundary and stays as text.
        let mut input = Input::default();
        type_str(&mut input, ":dog::cat:");
        assert_eq!(input.data, "🐶:cat:");
    }

    #[test]
    fn test_convert_emoji_in_middle_of_text() {
        // Cursor is moved back before the closing colon is typed, so text
        // trails the replacement range.
        let mut input = Input::default();
        type_str(&mut input, "x y");
        input.on_left();
        type_str(&mut input, ":dog:");
        assert_eq!(input.data, "x 🐶y");
        // cursor sits right after the emoji, before the trailing `y`
        assert_eq!(input.cursor, Cursor::new("x 🐶".len(), 0, 4));
    }

    /// Type `s` character by character without triggering emoji conversion, so a raw `:partial`
    /// shortcode can be built up for completion tests.
    fn put_str(input: &mut Input, s: &str) {
        for c in s.chars() {
            input.put_char(c);
        }
    }

    #[test]
    fn test_complete_emoji_unique_match() {
        // A partial with a single matching shortcode completes it fully.
        let mut input = Input::default();
        put_str(&mut input, ":thumbsu");
        assert!(input.try_complete_emoji());
        assert_eq!(input.data, ":thumbsup");
        // idx and col advance by the one extra character ("p").
        assert_eq!(input.cursor, Cursor::new(":thumbsup".len(), 0, 9));
        assert_eq!(input.completion_partial.as_deref(), Some("thumbsu"));
    }

    #[test]
    fn test_complete_emoji_cycles_matches() {
        // ":dog" matches ["dog", "dog2"]; current text "dog" is at index 0, so the first completion
        // moves to the next match.
        let mut input = Input::default();
        put_str(&mut input, ":dog");
        assert!(input.try_complete_emoji());
        assert_eq!(input.data, ":dog2");
        // The original partial is remembered across the cycle.
        assert_eq!(input.completion_partial.as_deref(), Some("dog"));

        // Cycling wraps back around to the first match.
        assert!(input.try_complete_emoji());
        assert_eq!(input.data, ":dog");
        assert_eq!(input.completion_partial.as_deref(), Some("dog"));
    }

    #[test]
    fn test_complete_emoji_partial_starts_first_match() {
        // "smil" is not itself a shortcode, so completion jumps to the first shortcode that starts
        // with it.
        let mut input = Input::default();
        put_str(&mut input, ":smil");
        assert!(input.try_complete_emoji());
        assert_eq!(input.data, ":smiley");
        assert_eq!(input.completion_partial.as_deref(), Some("smil"));
    }

    #[test]
    fn test_complete_emoji_no_match() {
        // No shortcode starts with "zzzz": nothing changes and state is reset.
        let mut input = Input::default();
        put_str(&mut input, ":zzzz");
        assert!(!input.try_complete_emoji());
        assert_eq!(input.data, ":zzzz");
        assert_eq!(input.completion_partial, None);
    }

    #[test]
    fn test_complete_emoji_no_colon() {
        // Without an opening colon there is nothing to complete.
        let mut input = Input::default();
        put_str(&mut input, "dog");
        assert!(!input.try_complete_emoji());
        assert_eq!(input.data, "dog");
        assert_eq!(input.completion_partial, None);
    }

    #[test]
    fn test_complete_emoji_empty_partial() {
        // A lone colon has an empty partial and does not complete.
        let mut input = Input::default();
        put_str(&mut input, ":");
        assert!(!input.try_complete_emoji());
        assert_eq!(input.data, ":");
        assert_eq!(input.completion_partial, None);
    }

    #[test]
    fn test_complete_emoji_empty_input() {
        let mut input = Input::default();
        assert!(!input.try_complete_emoji());
        assert_eq!(input.data, "");
        assert_eq!(input.completion_partial, None);
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn test_complete_mention_unique_match() {
        // The partial is matched ignoring case and completed to the full name.
        let mut input = Input::default();
        put_str(&mut input, "hi @al");
        assert!(input.try_complete_mention(&names(&["Alexi", "Bob"])));
        assert_eq!(input.data, "hi @Alexi");
        assert_eq!(input.cursor, Cursor::new("hi @Alexi".len(), 0, 9));
        assert_eq!(input.completion_partial.as_deref(), Some("al"));
    }

    #[test]
    fn test_complete_mention_cycles_matches() {
        let mut input = Input::default();
        put_str(&mut input, "@a");
        let candidates = names(&["Alexi", "Anna", "Bob"]);
        assert!(input.try_complete_mention(&candidates));
        assert_eq!(input.data, "@Alexi");
        assert!(input.try_complete_mention(&candidates));
        assert_eq!(input.data, "@Anna");
        // Cycling wraps back around to the first match.
        assert!(input.try_complete_mention(&candidates));
        assert_eq!(input.data, "@Alexi");
        assert_eq!(input.completion_partial.as_deref(), Some("a"));
    }

    #[test]
    fn test_complete_mention_empty_partial_lists_everyone() {
        let mut input = Input::default();
        put_str(&mut input, "@");
        let candidates = names(&["Alexi", "Bob"]);
        assert!(input.try_complete_mention(&candidates));
        assert_eq!(input.data, "@Alexi");
        assert!(input.try_complete_mention(&candidates));
        assert_eq!(input.data, "@Bob");
    }

    #[test]
    fn test_complete_mention_name_with_space_cycles() {
        // A completed name containing a space is still recognised as part of the cycle.
        let mut input = Input::default();
        put_str(&mut input, "@m");
        let candidates = names(&["Mary Jane", "Mike"]);
        assert!(input.try_complete_mention(&candidates));
        assert_eq!(input.data, "@Mary Jane");
        assert!(input.try_complete_mention(&candidates));
        assert_eq!(input.data, "@Mike");
    }

    #[test]
    fn test_complete_mention_partial_with_space() {
        // A partial can run past the first word of a name.
        let mut input = Input::default();
        put_str(&mut input, "hi @bob d");
        let candidates = names(&["Bob Dob", "Bob Smith"]);
        assert!(input.try_complete_mention(&candidates));
        assert_eq!(input.data, "hi @Bob Dob");
        assert_eq!(input.completion_partial.as_deref(), Some("bob d"));

        // A trailing space after the first name matches both people, so it cycles.
        let mut input = Input::default();
        put_str(&mut input, "@bob ");
        assert!(input.try_complete_mention(&candidates));
        assert_eq!(input.data, "@Bob Dob");
        assert!(input.try_complete_mention(&candidates));
        assert_eq!(input.data, "@Bob Smith");
    }

    #[test]
    fn test_complete_mention_keeps_text_after_cursor() {
        let mut input = Input::default();
        put_str(&mut input, "@al and more");
        for _ in 0.." and more".len() {
            input.on_left();
        }
        assert!(input.try_complete_mention(&names(&["Alexi"])));
        assert_eq!(input.data, "@Alexi and more");
        assert_eq!(input.cursor.idx, "@Alexi".len());
    }

    #[test]
    fn test_complete_mention_no_match() {
        let mut input = Input::default();
        put_str(&mut input, "@zz");
        assert!(!input.try_complete_mention(&names(&["Alexi"])));
        assert_eq!(input.data, "@zz");
    }

    #[test]
    fn test_complete_mention_ignores_email_and_plain_text() {
        let mut input = Input::default();
        put_str(&mut input, "mail me@al");
        assert!(!input.in_mention());
        assert!(!input.try_complete_mention(&names(&["Alexi"])));
        assert_eq!(input.data, "mail me@al");

        let mut input = Input::default();
        put_str(&mut input, "@alexi done");
        assert!(!input.try_complete_mention(&names(&["Alexi"])));
        assert_eq!(input.data, "@alexi done");
    }

    #[test]
    fn test_complete_mention_leaves_emoji_cycle_untouched() {
        // A failed mention completion must not reset an emoji completion cycle.
        let mut input = Input::default();
        put_str(&mut input, ":dog");
        assert!(input.try_complete_emoji());
        assert!(!input.try_complete_mention(&names(&["Alexi"])));
        assert_eq!(input.completion_partial.as_deref(), Some("dog"));
    }

    fn input_with_names(text: &str, mention_names: &[&str]) -> Input {
        let mut input = Input {
            mention_names: names(mention_names),
            ..Default::default()
        };
        put_str(&mut input, text);
        input
    }

    #[test]
    fn test_mention_spans() {
        let input = input_with_names("hi @Bob Dob, and @alexi!", &["Bob Dob", "Alexi"]);
        assert_eq!(input.mention_spans(), vec![3..11, 17..23]);
    }

    #[test]
    fn test_mention_spans_require_word_boundaries() {
        // `@` inside a word, and a name that is only the start of a longer word.
        assert!(
            input_with_names("me@Bob", &["Bob"])
                .mention_spans()
                .is_empty()
        );
        assert!(
            input_with_names("@Bobby", &["Bob"])
                .mention_spans()
                .is_empty()
        );
        // A shorter name is used when the longer one does not fit.
        let input = input_with_names("@Bob Dobson", &["Bob", "Bob Dob"]);
        assert_eq!(input.mention_spans(), vec![0..4]);
    }

    #[test]
    fn test_mention_spans_wait_for_longer_name() {
        // At the end of the input "@Bob" may become "@Bob Dob".
        let names = ["Bob", "Bob Dob"];
        assert!(input_with_names("@Bob", &names).mention_spans().is_empty());
        assert_eq!(
            input_with_names("@Bob ", &names).mention_spans(),
            vec![0..4]
        );
        assert_eq!(
            input_with_names("@Bob Dob", &names).mention_spans(),
            vec![0..8]
        );
    }

    #[test]
    fn test_backspace_deletes_whole_name_but_not_the_at() {
        // typed in full, in a different case than the name
        let mut input = input_with_names("hi @bob dob", &["Bob Dob"]);
        input.on_backspace();
        assert_eq!(input.data, "hi @");
        assert_eq!(input.cursor, Cursor::new(4, 0, 4));
        // the `@` is no longer a mention, so the next backspace deletes it
        input.on_backspace();
        assert_eq!(input.data, "hi ");

        // completed with tab
        let mut input = input_with_names("hi @bob d", &["Bob Dob"]);
        assert!(input.try_complete_mention(&input.mention_names.clone()));
        assert_eq!(input.data, "hi @Bob Dob");
        input.on_backspace();
        assert_eq!(input.data, "hi @");
    }

    #[test]
    fn test_backspace_edits_normally_outside_mentions() {
        let mut input = input_with_names("@Bob", &["Bob", "Bob Dob"]);
        input.on_backspace();
        assert_eq!(input.data, "@Bo");

        let mut input = input_with_names("@Bob Dob x", &["Bob Dob"]);
        input.on_backspace();
        assert_eq!(input.data, "@Bob Dob ");
    }

    #[test]
    fn test_delete_removes_whole_name_but_not_the_at() {
        let mut input = input_with_names("@Bob Dob x", &["Bob Dob"]);
        input.on_home();
        input.on_delete();
        assert_eq!(input.data, "@ x");
        assert_eq!(input.cursor, Cursor::new(0, 0, 0));
    }

    #[test]
    fn test_cursor_moves_over_mention() {
        let mut input = input_with_names("@Bob Dob x", &["Bob Dob"]);
        input.on_home();
        input.on_right();
        assert_eq!(input.cursor, Cursor::new(8, 0, 8));
        input.on_left();
        assert_eq!(input.cursor, Cursor::new(0, 0, 0));
    }

    #[test]
    fn test_word_movement_does_not_stop_inside_mention() {
        let mut input = input_with_names("@Bob Dob hello", &["Bob Dob"]);
        input.move_back_word();
        assert_eq!(input.cursor.idx, 9);
        input.move_back_word();
        assert_eq!(input.cursor.idx, 0);
        input.move_forward_word();
        assert_eq!(input.cursor.idx, 8);
    }

    #[test]
    fn test_delete_word_removes_whole_name_but_not_the_at() {
        let mut input = input_with_names("x @Bob Dob", &["Bob Dob"]);
        input.on_delete_word();
        assert_eq!(input.data, "x @");
    }
}
