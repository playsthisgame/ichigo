//! Single-line text editing with vim motions, shared by every form field.
//!
//! Fields stay plain `String`s. What lives here is the *caret* — and only for
//! the field that has focus, because that is the only one a keystroke can
//! reach. Each form keeps one [`Edit`] beside its focus index rather than a
//! caret per field; moving focus builds a fresh one with [`Edit::at_end`], so
//! tabbing into a field lands after its last character ready to type.
//!
//! Fields open in insert mode and `Esc` drops to normal mode, which is why
//! leaving a form now takes two `Esc`s — the first one is the vim one. Normal
//! mode is what makes `h`/`l`/`w`/`b` motions possible at all: in insert mode
//! those are letters someone is typing into a URL.
//!
//! Normal mode also walks *between* fields: `j` and `k` report
//! [`Applied::FocusNext`] / [`Applied::FocusPrev`] instead of moving a caret,
//! and the pane owning the form performs the move. It has to be reported rather
//! than done here — this module sees one `String` and knows nothing of rows,
//! action rows, or how many fields the pane has.
//!
//! Undo (`u`) lives here too, and its history therefore lives and dies with the
//! focused field's [`Edit`]: undo is per-field, and moving focus starts over.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};

use crate::config::Keys;

/// How long the first key of `keys.insert_escape` stays armed — vim's
/// `timeoutlen`. It exists so that typing `j`, pausing to think, then typing `k`
/// leaves you with `jk` in the field instead of silently dropping to normal
/// mode. It does not rescue a fast literal `jk`; that is the cost of the mapping
/// and the reason vim users pick a rare digraph.
const SEQUENCE_TIMEOUT: Duration = Duration::from_millis(1000);

/// Undo steps kept for one field. A focus change rebuilds the whole `Edit` and
/// so drops the history with it, which bounds this in practice; the cap only
/// exists so a very long session in a single field cannot grow without limit.
const MAX_UNDO: usize = 100;

/// The field as it stood before one change, for `u` to restore.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    value: String,
    caret: usize,
}

/// Where the caret is in the focused field, and whether letters type or move.
///
/// Not `Copy` — the undo history is a `Vec`. Only the renderer ever wanted a
/// copy, and it takes `&Edit` instead; every other holder already works through
/// `&mut`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Edit {
    /// Byte offset into the value, always on a char boundary. Equal to
    /// `value.len()` when the caret sits past the last character — which insert
    /// mode allows and normal mode does not, since vim's caret rests *on* a
    /// character.
    pub(super) caret: usize,
    /// False in normal mode, where letters move the caret instead of inserting.
    pub(super) insert: bool,
    /// The first key of `keys.insert_escape`, already inserted into the value
    /// and waiting to see whether the next key completes the sequence. Cleared
    /// by every other key, and by a focus change, since that rebuilds the whole
    /// `Edit` — which is what keeps the pending key from ever being deleted out
    /// of a field it was not typed into.
    pending: Option<(char, Instant)>,
    /// Whether the current insert session has already recorded its undo
    /// snapshot. What collapses a whole session into one `u`: the first
    /// keystroke that changes anything records, the rest do not.
    session_saved: bool,
    /// What `u` walks back through, oldest first. One entry per *change* — a
    /// whole insert session, or one normal-mode edit command — rather than one
    /// per keystroke, so undo reverses "the URL I just typed" instead of its
    /// last character. There is no redo stack: `u` pops.
    history: Vec<Snapshot>,
}

impl Default for Edit {
    /// An empty field, ready to type into.
    fn default() -> Self {
        Self::at_end("")
    }
}

impl Edit {
    /// The caret past the last character, in insert mode — what *opening* a
    /// pane on a field should give you.
    pub(super) fn at_end(value: &str) -> Self {
        Self {
            caret: value.len(),
            insert: true,
            pending: None,
            session_saved: false,
            history: Vec::new(),
        }
    }

    /// The caret at the end of `value`, in insert mode, or in normal mode with
    /// the caret resting on the last character.
    ///
    /// What a *focus move* lands with, and the reason it takes the flag rather
    /// than always opening in insert like [`at_end`](Self::at_end): the mode
    /// belongs to the user until they change it, so a walk begun in normal mode
    /// stays in normal mode. Landing in insert would make `jj` one move
    /// followed by a literal `j` typed into the field it landed on.
    pub(super) fn landing(value: &str, insert: bool) -> Self {
        let mut edit = Self::at_end(value);
        if !insert {
            edit.insert = false;
            rest_on_char(value, &mut edit);
        }
        edit
    }

    /// Records the field as it stands, before a change that is about to happen.
    ///
    /// Called at the point of mutation rather than when a session opens,
    /// because that is the only moment the *current* text is known for certain.
    /// Seeding at construction instead would put the burden on every caller of
    /// `at_end` and `default` to pass the right value — and `Edit::default()`,
    /// which several call sites use for a freshly added row, does not have one
    /// to pass. A wrong seed is not a cosmetic bug: `u` would blank the field.
    fn record(&mut self, value: &str) {
        // An insert session records once; everything typed between entering
        // insert mode and leaving it shares that one entry. A normal-mode
        // command is its own change and records every time.
        if self.insert {
            if self.session_saved {
                return;
            }
            self.session_saved = true;
        }
        self.snapshot(value);
    }

    fn snapshot(&mut self, value: &str) {
        // A change starting from text identical to the last snapshot would make
        // `u` look broken: it would restore what is already on screen.
        if self.history.last().is_some_and(|s| s.value == value) {
            return;
        }
        if self.history.len() == MAX_UNDO {
            self.history.remove(0);
        }
        self.history.push(Snapshot { value: value.to_string(), caret: self.caret });
    }
}

/// What the caller should do with a key handed to [`apply`].
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Applied {
    /// Consumed — the value or the caret moved, or the key was a motion with
    /// nowhere to go. Either way it must not fall through to a pane binding.
    Yes,
    /// `Esc` in normal mode: the field is done, so the pane should close. `Esc`
    /// in *insert* mode only leaves insert mode and reports `Yes`.
    Exit,
    /// `j` in normal mode: move focus to the next row of the form, wrapping.
    /// The pane owns the walk; all this module knows is that the key was a
    /// motion off the end of the field rather than text.
    FocusNext,
    /// `k` in normal mode: the previous row, wrapping.
    FocusPrev,
    /// Not a text-editing key — `Tab`, `Enter`, `Ctrl+<key>`. The caller's own
    /// match arms own these.
    No,
}

/// Applies one key to `value` at `edit`'s caret.
///
/// The caret is clamped against `value` on the way in, because a form can move
/// the value out from under it — a draft cloned into another pane and back, a
/// header row deleted at the focused index. Healing it here rather than at each
/// of those sites means a stale caret costs one harmless keystroke instead of
/// panicking on the next slice.
pub(super) fn apply(value: &mut String, edit: &mut Edit, key: KeyEvent, keys: Keys) -> Applied {
    // Ctrl+<letter> arrives as Char + CONTROL, so without this every unbound
    // one would type its letter. Alt likewise.
    if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        return Applied::No;
    }
    edit.caret = clamp(value, edit.caret);
    // Taken unconditionally: any key that is not the completing one disarms the
    // sequence, which is what stops a `j` typed here and a `k` typed after three
    // cursor moves from counting as `jk`.
    let pending = edit.pending.take();

    match key.code {
        // Arrows and Home/End work in both modes: they are unambiguous, so
        // there is no reason to make someone leave insert mode for them.
        KeyCode::Left => edit.caret = prev(value, edit.caret),
        KeyCode::Right => {
            edit.caret = next(value, edit.caret);
            rest_on_char(value, edit);
        }
        KeyCode::Home => edit.caret = 0,
        KeyCode::End => {
            edit.caret = value.len();
            rest_on_char(value, edit);
        }
        // Deletes in both modes rather than moving left in normal mode as vim
        // does: backspace-deletes is the one habit every text field shares, and
        // normal mode already has `x`.
        KeyCode::Backspace => {
            let from = prev(value, edit.caret);
            // Recorded inside the guard, so a Backspace at the start of the
            // field is not an undo step that restores identical text.
            if from < edit.caret {
                edit.record(value);
                value.replace_range(from..edit.caret, "");
                edit.caret = from;
            }
            rest_on_char(value, edit);
        }
        KeyCode::Delete => {
            let to = next(value, edit.caret);
            if to > edit.caret {
                edit.record(value);
                value.replace_range(edit.caret..to, "");
            }
            rest_on_char(value, edit);
        }
        KeyCode::Esc if edit.insert => {
            edit.insert = false;
            // Vim steps left when leaving insert at the end of a line.
            rest_on_char(value, edit);
        }
        KeyCode::Esc => return Applied::Exit,
        KeyCode::Char(c) if edit.insert => {
            if completes_escape(value, edit, keys, pending, c) {
                return Applied::Yes;
            }
            edit.record(value);
            value.insert(edit.caret, c);
            edit.caret += c.len_utf8();
            // Arm the sequence when this was its first key.
            if keys.insert_escape.is_some_and(|(first, _)| first == c) {
                edit.pending = Some((c, Instant::now()));
            }
        }
        KeyCode::Char(c) => return normal(value, edit, c),
        _ => return Applied::No,
    }
    Applied::Yes
}

/// Inserts `text` at the caret and leaves the caret after it — the paste path.
pub(super) fn insert_str(value: &mut String, edit: &mut Edit, text: &str) {
    edit.caret = clamp(value, edit.caret);
    // A paste disarms the sequence like any other non-completing input; pasting
    // a `k` after typing a `j` is not someone reaching for Esc.
    edit.pending = None;
    edit.record(value);
    value.insert_str(edit.caret, text);
    edit.caret += text.len();
}

/// Whether `c` completes `keys.insert_escape`, and if so un-types the pending
/// first key and drops to normal mode.
///
/// The character before the caret is checked against the pending one rather than
/// assumed: `pending` is armed the instant that key is inserted and disarmed by
/// everything else, but verifying costs one comparison and means a mismatch
/// degrades into a plain insert instead of deleting whatever happens to be
/// there.
fn completes_escape(
    value: &mut String,
    edit: &mut Edit,
    keys: Keys,
    pending: Option<(char, Instant)>,
    c: char,
) -> bool {
    let Some((first, second)) = keys.insert_escape else { return false };
    let Some((armed, at)) = pending else { return false };
    if c != second || armed != first || at.elapsed() > SEQUENCE_TIMEOUT {
        return false;
    }
    let from = prev(value, edit.caret);
    if !value[from..edit.caret].starts_with(first) {
        return false;
    }

    value.replace_range(from..edit.caret, "");
    edit.caret = from;
    edit.insert = false;
    rest_on_char(value, edit);
    true
}

/// One normal-mode key. Unrecognized letters are swallowed rather than passed
/// on: in normal mode a stray `q` is a mistyped motion, not a pane binding.
fn normal(value: &mut String, edit: &mut Edit, c: char) -> Applied {
    // Opening a session arms the *next* change to record. Commands that both
    // edit and enter insert mode (`s`, `C`, `S`) record their own edit below
    // and then mark the session saved, so the typing that follows joins it.
    if matches!(c, 'i' | 'a' | 'I' | 'A') {
        edit.session_saved = false;
    }

    match c {
        // Vim's `u`. Normal mode only — in insert mode it is a letter someone
        // is typing, which is also why it needs no guard here.
        'u' => {
            let Some(previous) = edit.history.pop() else { return Applied::Yes };
            *value = previous.value;
            edit.caret = clamp(value, previous.caret);
            rest_on_char(value, edit);
        }
        'h' => edit.caret = prev(value, edit.caret),
        'l' => {
            edit.caret = next(value, edit.caret);
            rest_on_char(value, edit);
        }
        'w' => edit.caret = word_forward(value, edit.caret, Words::Small),
        'W' => edit.caret = word_forward(value, edit.caret, Words::Big),
        'b' => edit.caret = word_back(value, edit.caret, Words::Small),
        'B' => edit.caret = word_back(value, edit.caret, Words::Big),
        'e' => edit.caret = word_end(value, edit.caret, Words::Small),
        'E' => edit.caret = word_end(value, edit.caret, Words::Big),
        '0' => edit.caret = 0,
        '^' => edit.caret = first_non_blank(value),
        '$' => {
            edit.caret = value.len();
            rest_on_char(value, edit);
        }
        // Off the field entirely, to the pane's next/previous row. Reported
        // rather than handled: which rows exist is the form's business. In
        // insert mode these are letters — and `j`/`k` in that order is the
        // default `insert_escape`, which is what puts you in normal mode to use
        // them.
        'j' => return Applied::FocusNext,
        'k' => return Applied::FocusPrev,
        'i' => edit.insert = true,
        'a' => {
            edit.caret = next(value, edit.caret);
            edit.insert = true;
        }
        'I' => {
            edit.caret = first_non_blank(value);
            edit.insert = true;
        }
        'A' => {
            edit.caret = value.len();
            edit.insert = true;
        }
        'x' => {
            let to = next(value, edit.caret);
            if to > edit.caret {
                edit.record(value);
                value.replace_range(edit.caret..to, "");
            }
            rest_on_char(value, edit);
        }
        's' => {
            let to = next(value, edit.caret);
            if to > edit.caret {
                edit.record(value);
                value.replace_range(edit.caret..to, "");
            }
            edit.insert = true;
            edit.session_saved = true;
        }
        'D' => {
            if edit.caret < value.len() {
                edit.record(value);
                value.truncate(edit.caret);
            }
            rest_on_char(value, edit);
        }
        'C' => {
            if edit.caret < value.len() {
                edit.record(value);
                value.truncate(edit.caret);
            }
            edit.insert = true;
            edit.session_saved = true;
        }
        // `cc`/`dd` would need a pending-operator state; `S` is the one-key
        // spelling of the same thing and is what a full-line rewrite wants.
        'S' => {
            if !value.is_empty() {
                edit.record(value);
                value.clear();
            }
            edit.caret = 0;
            edit.insert = true;
            edit.session_saved = true;
        }
        _ => {}
    }
    Applied::Yes
}

// ─── Caret arithmetic ─────────────────────────────────────────────────────────

/// Snaps a caret onto a char boundary no further than the end of `value`.
fn clamp(value: &str, caret: usize) -> usize {
    if caret >= value.len() {
        return value.len();
    }
    let mut caret = caret;
    while !value.is_char_boundary(caret) {
        caret -= 1;
    }
    caret
}

fn prev(value: &str, caret: usize) -> usize {
    value[..caret].chars().next_back().map_or(caret, |c| caret - c.len_utf8())
}

fn next(value: &str, caret: usize) -> usize {
    value[caret..].chars().next().map_or(caret, |c| caret + c.len_utf8())
}

/// Pulls a normal-mode caret back off the end of the line, where vim never
/// leaves it. A no-op in insert mode and on an empty value.
fn rest_on_char(value: &str, edit: &mut Edit) {
    if !edit.insert && edit.caret == value.len() {
        edit.caret = prev(value, edit.caret);
    }
}

fn first_non_blank(value: &str) -> usize {
    value
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map_or(0, |(i, _)| i)
}

// ─── Word motions ─────────────────────────────────────────────────────────────

/// Which of vim's two word shapes a motion uses: `w` treats a run of
/// punctuation as its own word, `W` only breaks on whitespace. In a URL that is
/// the difference between stepping through `https`, `://`, `host` and jumping
/// the whole thing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Words {
    Small,
    Big,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: char, words: Words) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if words == Words::Big || c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

/// `w`: past the current run, then past any whitespace. Lands on the end of the
/// line when there is no next word, which is where vim leaves it too.
fn word_forward(value: &str, caret: usize, words: Words) -> usize {
    let chars: Vec<(usize, char)> = value.char_indices().collect();
    let Some(mut i) = index_of(&chars, caret) else { return value.len() };

    let start = class(chars[i].1, words);
    if start != Class::Space {
        while i < chars.len() && class(chars[i].1, words) == start {
            i += 1;
        }
    }
    while i < chars.len() && class(chars[i].1, words) == Class::Space {
        i += 1;
    }
    chars.get(i).map_or(prev(value, value.len()), |(off, _)| *off)
}

/// `e`: the last character of the current run when the caret is before it,
/// otherwise of the next run.
fn word_end(value: &str, caret: usize, words: Words) -> usize {
    let chars: Vec<(usize, char)> = value.char_indices().collect();
    let Some(i) = index_of(&chars, caret) else { return value.len() };

    let mut i = i + 1;
    while i < chars.len() && class(chars[i].1, words) == Class::Space {
        i += 1;
    }
    if i >= chars.len() {
        return chars.last().map_or(0, |(off, _)| *off);
    }
    let run = class(chars[i].1, words);
    while i + 1 < chars.len() && class(chars[i + 1].1, words) == run {
        i += 1;
    }
    chars[i].0
}

/// `b`: back over any whitespace, then to the start of that run.
fn word_back(value: &str, caret: usize, words: Words) -> usize {
    let chars: Vec<(usize, char)> = value.char_indices().collect();
    // Starting past the end (an insert-mode caret at EOL) means the last
    // character is the one to walk back from.
    let start = index_of(&chars, caret).unwrap_or(chars.len());
    if start == 0 {
        return 0;
    }

    let mut i = start - 1;
    while i > 0 && class(chars[i].1, words) == Class::Space {
        i -= 1;
    }
    if class(chars[i].1, words) == Class::Space {
        return chars[i].0;
    }
    let run = class(chars[i].1, words);
    while i > 0 && class(chars[i - 1].1, words) == run {
        i -= 1;
    }
    chars[i].0
}

/// Turns a byte caret into an index into `chars`; `None` when it is past the
/// end.
fn index_of(chars: &[(usize, char)], caret: usize) -> Option<usize> {
    chars.iter().position(|(off, _)| *off == caret)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    fn code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }

    /// Types `presses` into a field starting from `edit`, with no insert-escape
    /// sequence configured.
    fn run(value: &str, edit: Edit, presses: &[KeyEvent]) -> (String, Edit) {
        run_with(value, edit, presses, Keys::default())
    }

    fn run_with(value: &str, edit: Edit, presses: &[KeyEvent], keys: Keys) -> (String, Edit) {
        let mut value = value.to_string();
        let mut edit = edit;
        for k in presses {
            apply(&mut value, &mut edit, *k, keys);
        }
        (value, edit)
    }

    fn normal_at(caret: usize) -> Edit {
        Edit { caret, insert: false, ..Edit::default() }
    }

    /// `inoremap jk <Esc>`.
    fn jk() -> Keys {
        Keys { insert_escape: Some(('j', 'k')) }
    }

    #[test]
    fn insert_lands_at_the_caret_not_the_end() {
        let (value, edit) = run("world", Edit { caret: 0, insert: true, ..Edit::default() }, &[key('h'), key('i')]);
        assert_eq!(value, "hiworld");
        assert_eq!(edit.caret, 2);
    }

    #[test]
    fn esc_enters_normal_mode_then_exits() {
        let mut value = "x".to_string();
        let mut edit = Edit::at_end(&value);
        assert_eq!(apply(&mut value, &mut edit, code(KeyCode::Esc), Keys::default()), Applied::Yes);
        assert!(!edit.insert);
        // Vim steps off the end when leaving insert mode.
        assert_eq!(edit.caret, 0);
        assert_eq!(apply(&mut value, &mut edit, code(KeyCode::Esc), Keys::default()), Applied::Exit);
    }

    #[test]
    fn normal_mode_letters_move_instead_of_typing() {
        let (value, edit) = run("abc", normal_at(0), &[key('l'), key('l')]);
        assert_eq!(value, "abc");
        assert_eq!(edit.caret, 2);
        // `l` stops on the last character rather than past it.
        let (_, edit) = run("abc", normal_at(2), &[key('l')]);
        assert_eq!(edit.caret, 2);
    }

    #[test]
    fn h_and_l_stop_at_the_edges() {
        let (_, edit) = run("abc", normal_at(0), &[key('h')]);
        assert_eq!(edit.caret, 0);
    }

    #[test]
    fn small_words_step_through_url_punctuation() {
        let url = "https://api.example.com/v1";
        // https → :// → api → . → example …
        let stops: Vec<usize> = std::iter::successors(Some(0), |c| {
            let next = word_forward(url, *c, Words::Small);
            if next == *c { None } else { Some(next) }
        })
        .collect();
        assert_eq!(&url[stops[1]..stops[1] + 3], "://");
        assert_eq!(&url[stops[2]..stops[2] + 3], "api");
    }

    #[test]
    fn big_words_jump_the_whole_url() {
        let value = "GET https://x/y now";
        assert_eq!(word_forward(value, 0, Words::Big), 4);
        assert_eq!(word_forward(value, 4, Words::Big), 16);
    }

    #[test]
    fn b_returns_to_the_start_of_the_previous_word() {
        let value = "one two three";
        assert_eq!(word_back(value, 8, Words::Small), 4);
        assert_eq!(word_back(value, 4, Words::Small), 0);
        assert_eq!(word_back(value, 0, Words::Small), 0);
    }

    #[test]
    fn b_from_past_the_end_walks_back_from_the_last_char() {
        let value = "one two";
        assert_eq!(word_back(value, value.len(), Words::Small), 4);
    }

    #[test]
    fn e_lands_on_the_last_char_of_the_next_word() {
        let value = "one two";
        assert_eq!(word_end(value, 0, Words::Small), 2);
        assert_eq!(word_end(value, 2, Words::Small), 6);
    }

    #[test]
    fn word_motions_on_an_empty_value_stay_put() {
        assert_eq!(word_forward("", 0, Words::Small), 0);
        assert_eq!(word_back("", 0, Words::Small), 0);
        assert_eq!(word_end("", 0, Words::Small), 0);
    }

    #[test]
    fn x_deletes_under_the_caret_and_never_past_the_end() {
        let (value, edit) = run("abc", normal_at(2), &[key('x'), key('x'), key('x'), key('x')]);
        assert_eq!(value, "");
        assert_eq!(edit.caret, 0);
    }

    #[test]
    fn dollar_c_rewrites_the_tail() {
        let (value, edit) = run(
            "https://old/path",
            normal_at(0),
            &[key('$'), key('C'), key('X')],
        );
        assert_eq!(value, "https://old/patX");
        assert!(edit.insert);
    }

    #[test]
    fn a_appends_after_the_caret() {
        let (value, _) = run("ab", normal_at(1), &[key('a'), key('!')]);
        assert_eq!(value, "ab!");
    }

    #[test]
    fn backspace_deletes_before_the_caret_in_both_modes() {
        let (value, edit) = run("abc", Edit { caret: 2, insert: true, ..Edit::default() }, &[code(KeyCode::Backspace)]);
        assert_eq!(value, "ac");
        assert_eq!(edit.caret, 1);

        let (value, _) = run("abc", normal_at(2), &[code(KeyCode::Backspace)]);
        assert_eq!(value, "ac");
    }

    #[test]
    fn multibyte_values_move_by_character() {
        let mut value = "aé…b".to_string();
        let mut edit = Edit::at_end(&value);
        for _ in 0..4 {
            apply(&mut value, &mut edit, code(KeyCode::Left), Keys::default());
        }
        assert_eq!(edit.caret, 0);
        apply(&mut value, &mut edit, code(KeyCode::Right), Keys::default());
        assert_eq!(edit.caret, 1);
        apply(&mut value, &mut edit, code(KeyCode::Delete), Keys::default());
        assert_eq!(value, "a…b");
    }

    #[test]
    fn a_stale_caret_is_clamped_rather_than_panicking() {
        let mut value = "hi".to_string();
        let mut edit = Edit { caret: 99, insert: true, ..Edit::default() };
        apply(&mut value, &mut edit, key('!'), Keys::default());
        assert_eq!(value, "hi!");

        // Mid-character carets snap back to a boundary.
        let mut value = "é".to_string();
        let mut edit = Edit { caret: 1, insert: true, ..Edit::default() };
        apply(&mut value, &mut edit, key('x'), Keys::default());
        assert_eq!(value, "xé");
    }

    #[test]
    fn ctrl_keys_are_left_to_the_caller() {
        let mut value = String::new();
        let mut edit = Edit::default();
        let ctrl_h = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert_eq!(apply(&mut value, &mut edit, ctrl_h, Keys::default()), Applied::No);
        assert_eq!(value, "");
    }

    #[test]
    fn u_undoes_everything_typed_since_the_field_was_focused() {
        // A field opens in insert mode, so the first change is already underway
        // and the seeded snapshot is what `u` has to fall back on.
        let presses = [key('/'), key('v'), key('2'), code(KeyCode::Esc), key('u')];
        let (value, edit) = run("https://x", Edit::at_end("https://x"), &presses);
        assert_eq!(value, "https://x");
        assert!(!edit.insert);
    }

    #[test]
    fn one_insert_session_is_one_undo_step_not_one_per_keystroke() {
        let mut value = "a".to_string();
        let mut edit = Edit::at_end(&value);
        for k in [key('b'), key('c'), key('d'), code(KeyCode::Esc)] {
            apply(&mut value, &mut edit, k, Keys::default());
        }
        assert_eq!(value, "abcd");
        apply(&mut value, &mut edit, key('u'), Keys::default());
        assert_eq!(value, "a", "one `u` should undo the whole session");
    }

    #[test]
    fn successive_changes_undo_one_at_a_time_in_reverse() {
        let mut value = "one".to_string();
        let mut edit = Edit::at_end(&value);
        // Session 1: append " two". Session 2: append " three".
        for k in [key(' '), key('t'), key('w'), key('o'), code(KeyCode::Esc)] {
            apply(&mut value, &mut edit, k, Keys::default());
        }
        for k in [key('A'), key('!'), code(KeyCode::Esc)] {
            apply(&mut value, &mut edit, k, Keys::default());
        }
        assert_eq!(value, "one two!");

        apply(&mut value, &mut edit, key('u'), Keys::default());
        assert_eq!(value, "one two");
        apply(&mut value, &mut edit, key('u'), Keys::default());
        assert_eq!(value, "one");
    }

    #[test]
    fn u_on_an_exhausted_history_leaves_the_field_alone() {
        let mut value = "abc".to_string();
        let mut edit = normal_at(0);
        for _ in 0..5 {
            apply(&mut value, &mut edit, key('u'), Keys::default());
        }
        assert_eq!(value, "abc");
    }

    #[test]
    fn normal_mode_edits_are_each_their_own_undo_step() {
        let mut value = "abcd".to_string();
        let mut edit = normal_at(0);
        apply(&mut value, &mut edit, key('x'), Keys::default());
        apply(&mut value, &mut edit, key('x'), Keys::default());
        assert_eq!(value, "cd");
        apply(&mut value, &mut edit, key('u'), Keys::default());
        assert_eq!(value, "bcd");
        apply(&mut value, &mut edit, key('u'), Keys::default());
        assert_eq!(value, "abcd");
    }

    #[test]
    fn undo_restores_the_caret_with_the_text() {
        let mut value = "abc".to_string();
        let mut edit = normal_at(2);
        apply(&mut value, &mut edit, key('D'), Keys::default());
        assert_eq!(value, "ab");
        apply(&mut value, &mut edit, key('u'), Keys::default());
        assert_eq!(value, "abc");
        assert_eq!(edit.caret, 2);
    }

    #[test]
    fn a_change_that_alters_nothing_does_not_add_an_undo_step() {
        // `i` then straight back out edits nothing; `u` should still reach the
        // change *before* it rather than appearing to do nothing.
        let mut value = "ab".to_string();
        let mut edit = Edit::at_end(&value);
        for k in [key('c'), code(KeyCode::Esc), key('i'), code(KeyCode::Esc)] {
            apply(&mut value, &mut edit, k, Keys::default());
        }
        assert_eq!(value, "abc");
        apply(&mut value, &mut edit, key('u'), Keys::default());
        assert_eq!(value, "ab");
    }

    #[test]
    fn u_is_a_literal_character_in_insert_mode() {
        let (value, _) = run("", Edit::default(), &[key('u'), key('u')]);
        assert_eq!(value, "uu");
    }

    #[test]
    fn the_history_is_capped_and_drops_its_oldest_entry() {
        let mut edit = Edit::default();
        for i in 0..MAX_UNDO + 10 {
            edit.snapshot(&format!("v{i}"));
        }
        assert_eq!(edit.history.len(), MAX_UNDO);
        assert_eq!(edit.history[0].value, format!("v{}", 10));
    }

    #[test]
    fn the_configured_sequence_leaves_insert_mode_and_un_types_itself() {
        let (value, edit) = run_with("ab", Edit::at_end("ab"), &[key('j'), key('k')], jk());
        assert_eq!(value, "ab");
        assert!(!edit.insert);
        // Normal mode rests on a character, as after a plain Esc.
        assert_eq!(edit.caret, 1);
    }

    #[test]
    fn the_sequence_does_nothing_when_it_is_not_configured() {
        let (value, edit) = run("ab", Edit::at_end("ab"), &[key('j'), key('k')]);
        assert_eq!(value, "abjk");
        assert!(edit.insert);
    }

    #[test]
    fn the_first_key_alone_stays_in_the_field() {
        let (value, edit) = run_with("ab", Edit::at_end("ab"), &[key('j')], jk());
        assert_eq!(value, "abj");
        assert!(edit.insert);
    }

    #[test]
    fn a_key_between_the_two_disarms_the_sequence() {
        // `j`, Left, `k` is someone editing, not someone reaching for Esc.
        let presses = [key('j'), code(KeyCode::Left), key('k')];
        let (value, edit) = run_with("ab", Edit::at_end("ab"), &presses, jk());
        assert_eq!(value, "abkj");
        assert!(edit.insert);
    }

    #[test]
    fn only_the_configured_second_key_completes_it() {
        let (value, edit) = run_with("", Edit::default(), &[key('j'), key('x')], jk());
        assert_eq!(value, "jx");
        assert!(edit.insert);
    }

    #[test]
    fn the_sequence_expires_after_the_timeout() {
        let mut value = "ab".to_string();
        let mut edit = Edit::at_end(&value);
        apply(&mut value, &mut edit, key('j'), jk());
        // Backdate the arming past `timeoutlen`: a pause then `k` is a literal.
        edit.pending = edit
            .pending
            .map(|(c, at)| (c, at - SEQUENCE_TIMEOUT - Duration::from_millis(1)));
        apply(&mut value, &mut edit, key('k'), jk());
        assert_eq!(value, "abjk");
        assert!(edit.insert);
    }

    #[test]
    fn the_sequence_is_inert_in_normal_mode() {
        // `j` and `k` are motions there; neither should re-trigger an escape.
        let (value, edit) = run_with("abc", normal_at(0), &[key('j'), key('k')], jk());
        assert_eq!(value, "abc");
        assert!(!edit.insert);
    }

    #[test]
    fn a_sequence_typed_mid_value_removes_only_its_own_first_key() {
        let start = Edit { caret: 1, insert: true, ..Edit::default() };
        let (value, edit) = run_with("ac", start, &[key('j'), key('k')], jk());
        assert_eq!(value, "ac");
        assert_eq!(edit.caret, 1);
    }

    #[test]
    fn a_paste_between_the_two_keys_disarms_the_sequence() {
        let mut value = String::new();
        let mut edit = Edit::default();
        apply(&mut value, &mut edit, key('j'), jk());
        insert_str(&mut value, &mut edit, "X");
        apply(&mut value, &mut edit, key('k'), jk());
        assert_eq!(value, "jXk");
        assert!(edit.insert);
    }

    #[test]
    fn paste_lands_at_the_caret() {
        let mut value = "ac".to_string();
        let mut edit = Edit { caret: 1, insert: true, ..Edit::default() };
        insert_str(&mut value, &mut edit, "b");
        assert_eq!(value, "abc");
        assert_eq!(edit.caret, 2);
    }

    /// `j`/`k` in normal mode leave the field alone and hand the walk to the
    /// pane; nothing about the value or the caret may move on the way out.
    #[test]
    fn normal_mode_j_and_k_report_a_row_walk() {
        for (press, expected) in [('j', Applied::FocusNext), ('k', Applied::FocusPrev)] {
            let mut value = "https://x".to_string();
            let mut edit = normal_at(4);
            assert_eq!(apply(&mut value, &mut edit, key(press), Keys::default()), expected);
            assert_eq!(value, "https://x");
            assert_eq!(edit.caret, 4);
            assert!(!edit.insert);
        }
    }

    /// The same two keys in insert mode are text — which is what `insert_escape`
    /// exists to get you out of, and the only reason the walk is reachable.
    #[test]
    fn insert_mode_j_and_k_are_still_characters() {
        let (value, edit) = run("", Edit::default(), &[key('j'), key('k')]);
        assert_eq!(value, "jk");
        assert!(edit.insert);
    }

    /// A field taken by a walk that began in normal mode stays in normal mode,
    /// with the caret resting on the last character rather than past it.
    #[test]
    fn landing_carries_the_mode_it_is_given() {
        let insert = Edit::landing("abc", true);
        assert_eq!(insert.caret, 3);
        assert!(insert.insert);

        let resting = Edit::landing("abc", false);
        assert_eq!(resting.caret, 2);
        assert!(!resting.insert);

        // Nothing to rest on, and no caret to pull back off the end.
        assert_eq!(Edit::landing("", false).caret, 0);
    }
}
