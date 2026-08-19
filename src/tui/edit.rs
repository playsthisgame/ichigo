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
//!
//! The commands that take a second key — the `d`/`c`/`y` operators, `f`/`F`/
//! `t`/`T`, `r`, `gg` — are one `Pending` value on the [`Edit`], armed by the
//! first key and read by the next. [`apply`] *takes* it on the way in, so every
//! key that does not reach the normal-mode table cancels it; that is the same
//! trick the insert-escape sequence uses one field over, and for the same
//! reason: a `d` left armed across an arrow key would delete a word the next
//! time someone typed `w`. Both operators and plain motions read one table,
//! [`simple_motion`], so `w` and `dw` cannot drift apart.
//!
//! The register `p` puts is **not** here: it is one `String` on `App`, so a
//! bearer token yanked out of one header can be put into another. Undo is
//! per-field because a field's history is only meaningful against that field's
//! text; a register is just text, and scoping it the same way would have made
//! `y` and `p` useless together.
//!
//! One field is not single-line: the request body. [`apply_multiline`] is the
//! door for it — Enter becomes a newline, and `j`/`k` move by line before they
//! fall off the field and become a row walk. Everything else it hands straight
//! to [`apply`], which is why the motions that are *about* a line (`0`, `^`,
//! `$`, `D`, `C`, `S`) are measured against the line the caret is on rather
//! than against the whole value. On a value with no newline in it the two are
//! the same string, so every single-line field behaves exactly as before.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::ops::Range;
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
    /// A normal-mode command that has been typed but not completed — `d`
    /// waiting for its motion, `f` waiting for the character to search for,
    /// `r` for the replacement, `g` for its second `g`.
    ///
    /// Separate from [`pending`](Self::pending), which is the insert-escape
    /// sequence: the two can never be armed at once, since one only arms in
    /// insert mode and the other only in normal mode, but keeping them apart
    /// means neither has to reason about the other's lifetime. Cleared by the
    /// key that completes it and by any key that cannot.
    pending_cmd: Option<Pending>,
    /// The last `f`/`F`/`t`/`T` performed, for `;` and `,` to repeat. Lives in
    /// the `Edit` like the undo history, and dies with it for the same reason.
    last_find: Option<(FindKind, char)>,
}

/// A normal-mode command waiting for the key that finishes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pending {
    /// `d`, `c` or `y`, waiting for the motion naming its span.
    Operator(Op),
    /// The same, after its motion turned out to be `f`/`F`/`t`/`T` — now
    /// waiting for the character that motion searches for.
    OperatorFind(Op, FindKind),
    /// `f`/`F`/`t`/`T` on its own, waiting for its target character.
    Find(FindKind),
    /// `r`, waiting for the character to write over the one under the caret.
    Replace,
    /// `g`, waiting for the second one. `gg` is the only `g` command bound, so
    /// anything else cancels.
    G,
}

/// What an operator does with the span its motion names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Delete,
    Change,
    Yank,
}

/// Which way an `f`-family motion searches, and whether it stops on its target
/// or one short of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FindKind {
    forward: bool,
    till: bool,
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
            pending_cmd: None,
            last_find: None,
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
pub(super) fn apply(
    value: &mut String,
    edit: &mut Edit,
    key: KeyEvent,
    keys: Keys,
    register: &mut String,
) -> Applied {
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
    // Likewise for a half-typed normal-mode command: everything below except
    // the normal-mode arm drops it, so `d`, an arrow key, then `w` moves a word
    // instead of deleting one.
    let pending_cmd = edit.pending_cmd.take();

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
        KeyCode::Char(c) => return normal(value, edit, c, pending_cmd, register),
        _ => return Applied::No,
    }
    Applied::Yes
}

/// Applies one key to a field that may hold newlines — the request body, and
/// nothing else.
///
/// It is a wrapper rather than a flag on [`apply`] because only two keys
/// actually differ, and both of them differ in the direction of *more* text
/// editing rather than less:
///
/// * `Enter` inserts a newline instead of falling through to the pane. It has
///   to: a terminal without bracketed paste delivers a pasted JSON body as
///   characters with Enters between the lines, so a pane whose Enter meant
///   "apply" would keep the first line and throw the rest at its own bindings.
///   That is the same contract the cURL import buffer has, and the reason both
///   are applied with `Ctrl+s`.
/// * `j` and `k` move by line while there is a line to move to, and only report
///   [`Applied::FocusNext`] / [`Applied::FocusPrev`] off the top and bottom. In
///   a one-line field there never is one, so the row walk is unchanged; in a
///   twenty-line body, `j` walking to the next *row* would be unusable.
///
/// Everything else — insert, motions, undo, the insert-escape sequence — is
/// [`apply`]'s, so the body field cannot drift away from the fields around it.
pub(super) fn apply_multiline(
    value: &mut String,
    edit: &mut Edit,
    key: KeyEvent,
    keys: Keys,
    register: &mut String,
) -> Applied {
    if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
        return Applied::No;
    }
    edit.caret = clamp(value, edit.caret);

    match key.code {
        KeyCode::Enter if edit.insert => {
            edit.pending_cmd = None;
            insert_str(value, edit, "\n");
        }
        // Normal mode: vim's Enter is a line down, to the first non-blank.
        KeyCode::Enter => {
            edit.pending_cmd = None;
            let Some(caret) = line_down(value, edit.caret) else { return Applied::Yes };
            edit.caret = first_non_blank(value, caret);
            rest_on_char(value, edit);
        }
        KeyCode::Down => return vertical(value, edit, true),
        KeyCode::Up => return vertical(value, edit, false),
        KeyCode::Char('j') if !edit.insert => return vertical(value, edit, true),
        KeyCode::Char('k') if !edit.insert => return vertical(value, edit, false),
        _ => return apply(value, edit, key, keys, register),
    }
    Applied::Yes
}

/// One line down or up, or the row walk when there is no such line.
///
/// These are the keys `apply_multiline` keeps for itself, so they are also
/// where a half-typed operator has to be dropped: `d` then `j` is not a
/// linewise delete here, it is a `d` that never found its motion.
fn vertical(value: &str, edit: &mut Edit, down: bool) -> Applied {
    edit.pending_cmd = None;
    let moved = if down { line_down(value, edit.caret) } else { line_up(value, edit.caret) };
    match moved {
        Some(caret) => {
            edit.caret = caret;
            rest_on_char(value, edit);
            Applied::Yes
        }
        None if down => Applied::FocusNext,
        None => Applied::FocusPrev,
    }
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
///
/// `pending_cmd` is the half-typed command this key might complete — the `d` of
/// a `dw`, the `f` of an `fx`. It arrives as an argument rather than being read
/// off `edit` because [`apply`] takes it on the way in, which is what makes
/// every key that does not reach here cancel it.
fn normal(
    value: &mut String,
    edit: &mut Edit,
    c: char,
    pending_cmd: Option<Pending>,
    register: &mut String,
) -> Applied {
    if let Some(pending) = pending_cmd {
        return complete(value, edit, c, pending, register);
    }

    // Opening a session arms the *next* change to record. Commands that both
    // edit and enter insert mode (`s`, `C`, `S`, and the `c` operator) record
    // their own edit below and then mark the session saved, so the typing that
    // follows joins it.
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
        // The commands that wait for one more key. Armed here and finished by
        // `complete` on the next press.
        'd' => edit.pending_cmd = Some(Pending::Operator(Op::Delete)),
        'c' => edit.pending_cmd = Some(Pending::Operator(Op::Change)),
        'y' => edit.pending_cmd = Some(Pending::Operator(Op::Yank)),
        'r' => edit.pending_cmd = Some(Pending::Replace),
        'g' => edit.pending_cmd = Some(Pending::G),
        'f' | 'F' | 't' | 'T' => edit.pending_cmd = Some(Pending::Find(find_kind(c))),
        // `Y` is vim's `yy`: the whole line, which for every field but the body
        // is the whole value.
        'Y' => {
            let (start, end) = line_span(value, edit.caret);
            if end > start {
                *register = value[start..end].to_string();
            }
        }
        // The last line's first non-blank, which in a single-line field is the
        // only line's — the same place `^` goes, exactly as in vim.
        'G' => {
            edit.caret = first_non_blank(value, last_line_start(value));
            rest_on_char(value, edit);
        }
        // The register is charwise and only charwise — see `cut`. `p` puts it
        // after the character under the caret, `P` before it, and both leave
        // the caret on the last character put, as vim does.
        'p' | 'P' => {
            if register.is_empty() {
                return Applied::Yes;
            }
            let at = if c == 'p' { next(value, edit.caret) } else { edit.caret };
            edit.record(value);
            value.insert_str(at, register);
            edit.caret = prev(value, at + register.len());
            rest_on_char(value, edit);
        }
        // `~`: swap the case of the character under the caret and step past it.
        // The swapped text is measured rather than assumed one character long,
        // since a few characters change byte length when their case does.
        '~' => {
            let to = next(value, edit.caret);
            if to > edit.caret && !value[edit.caret..].starts_with('\n') {
                let swapped: String = value[edit.caret..to].chars().flat_map(swap_case).collect();
                edit.record(value);
                value.replace_range(edit.caret..to, &swapped);
                edit.caret += swapped.len();
            }
            rest_on_char(value, edit);
        }
        // `;` and `,` repeat the last `f`/`F`/`t`/`T`, forwards and backwards.
        ';' | ',' => {
            let Some((kind, target)) = edit.last_find else { return Applied::Yes };
            let kind = if c == ',' { kind.reversed() } else { kind };
            if let Some((caret, _)) = find_motion(value, edit.caret, kind, target) {
                edit.caret = caret;
                rest_on_char(value, edit);
            }
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
            edit.caret = first_non_blank(value, edit.caret);
            edit.insert = true;
        }
        'A' => {
            edit.caret = line_end(value, edit.caret);
            edit.insert = true;
        }
        'x' => {
            cut(value, edit, edit.caret..next(value, edit.caret), register);
            rest_on_char(value, edit);
        }
        's' => {
            cut(value, edit, edit.caret..next(value, edit.caret), register);
            edit.insert = true;
            edit.session_saved = true;
        }
        'D' => {
            cut(value, edit, edit.caret..line_end(value, edit.caret), register);
            rest_on_char(value, edit);
        }
        'C' => {
            cut(value, edit, edit.caret..line_end(value, edit.caret), register);
            edit.insert = true;
            edit.session_saved = true;
        }
        // `S` is the one-key spelling of `cc`, and predates it. Both are bound;
        // this is the one that needs no operator to type.
        'S' => {
            let (start, end) = line_span(value, edit.caret);
            edit.caret = start;
            cut(value, edit, start..end, register);
            edit.insert = true;
            edit.session_saved = true;
        }
        // Everything left is either a motion from the shared table or a
        // mistyped key, and both are swallowed: in normal mode a stray `q` is
        // not a pane binding.
        _ => {
            if let Some((caret, _)) = simple_motion(value, edit.caret, c) {
                edit.caret = caret;
                rest_on_char(value, edit);
            }
        }
    }
    Applied::Yes
}

/// The key that finishes a half-typed command.
///
/// Anything that cannot finish it cancels: `pending_cmd` was already taken by
/// [`apply`], so returning without re-arming *is* the cancellation. The key is
/// not then re-read as a fresh command — vim discards it too, and re-reading
/// would make a mistyped `dq` delete something on the next keystroke.
fn complete(
    value: &mut String,
    edit: &mut Edit,
    c: char,
    pending: Pending,
    register: &mut String,
) -> Applied {
    match pending {
        // `gg`: the first line's first non-blank.
        Pending::G => {
            if c == 'g' {
                edit.caret = first_non_blank(value, 0);
                rest_on_char(value, edit);
            }
        }
        // `r<char>`. Not allowed to write over the newline ending a line: that
        // would join two lines, which is `J`'s job and not `r`'s.
        Pending::Replace => {
            let to = next(value, edit.caret);
            if to > edit.caret && !value[edit.caret..].starts_with('\n') {
                edit.record(value);
                value.replace_range(edit.caret..to, c.encode_utf8(&mut [0; 4]));
            }
            rest_on_char(value, edit);
        }
        // `f<char>` and friends on their own: a motion, and the one every later
        // `;` repeats. The search is remembered even when it fails, so that a
        // `;` after a miss looks for the same character rather than the one
        // before it.
        Pending::Find(kind) => {
            edit.last_find = Some((kind, c));
            if let Some((caret, _)) = find_motion(value, edit.caret, kind, c) {
                edit.caret = caret;
                rest_on_char(value, edit);
            }
        }
        // `df<char>`: the same search, with an operator waiting on its answer.
        Pending::OperatorFind(op, kind) => {
            edit.last_find = Some((kind, c));
            if let Some((target, inclusive)) = find_motion(value, edit.caret, kind, c) {
                let span = op_span(value, edit.caret, target, inclusive);
                return operate(value, edit, op, span, register);
            }
        }
        Pending::Operator(op) => return operator_motion(value, edit, c, op, register),
    }
    Applied::Yes
}

/// The key after `d`, `c` or `y`.
fn operator_motion(
    value: &mut String,
    edit: &mut Edit,
    c: char,
    op: Op,
    register: &mut String,
) -> Applied {
    // `dd`, `cc`, `yy`: the operator doubled is its own line. Charwise even so
    // — see `cut` — so it takes the line's text and leaves the line itself.
    if c == op.letter() {
        let (start, end) = line_span(value, edit.caret);
        edit.caret = start;
        return operate(value, edit, op, start..end, register);
    }
    // A search still needs its target character, so the operator waits one more
    // key rather than resolving anything here.
    if matches!(c, 'f' | 'F' | 't' | 'T') {
        edit.pending_cmd = Some(Pending::OperatorFind(op, find_kind(c)));
        return Applied::Yes;
    }
    if matches!(c, ';' | ',') {
        let Some((kind, target)) = edit.last_find else { return Applied::Yes };
        let kind = if c == ',' { kind.reversed() } else { kind };
        let Some((to, inclusive)) = find_motion(value, edit.caret, kind, target) else {
            return Applied::Yes;
        };
        let span = op_span(value, edit.caret, to, inclusive);
        return operate(value, edit, op, span, register);
    }
    // `cw` is vim's one irregular operator-motion pair: on a non-blank it acts
    // like `ce`, so that changing a word does not swallow the space after it.
    let end_instead = op == Op::Change
        && matches!(c, 'w' | 'W')
        && value[edit.caret..].starts_with(|ch: char| !ch.is_whitespace());
    let key = match (end_instead, c) {
        (true, 'w') => 'e',
        (true, 'W') => 'E',
        _ => c,
    };
    let Some((target, inclusive)) = simple_motion(value, edit.caret, key) else {
        return Applied::Yes;
    };
    let span = op_span(value, edit.caret, target, inclusive);
    operate(value, edit, op, span, register)
}

/// Runs `op` over `span`, a byte range whose ends are both char boundaries.
fn operate(
    value: &mut String,
    edit: &mut Edit,
    op: Op,
    span: Range<usize>,
    register: &mut String,
) -> Applied {
    match op {
        // Vim leaves the caret at the start of what was yanked.
        Op::Yank => {
            if !span.is_empty() {
                *register = value[span.clone()].to_string();
                edit.caret = span.start;
            }
            rest_on_char(value, edit);
        }
        Op::Delete => {
            edit.caret = span.start;
            cut(value, edit, span, register);
            rest_on_char(value, edit);
        }
        Op::Change => {
            edit.caret = span.start;
            cut(value, edit, span, register);
            edit.insert = true;
            edit.session_saved = true;
        }
    }
    Applied::Yes
}

/// Removes `span` from `value`, recording the undo step and filling the
/// register with what came out.
///
/// The register is **charwise and only charwise**, `dd` and `yy` included:
/// every field but the body is a single line that must never gain a newline,
/// and a linewise register would put one there the first time someone yanked a
/// line in the body and put it in a URL. The cost is that `yy` then `p` inserts
/// the line's text at the caret rather than opening a line below it.
///
/// Recording sits inside the emptiness guard for the reason every other
/// mutation site's does: an undo step that restores identical text reads as a
/// broken `u`.
fn cut(value: &mut String, edit: &mut Edit, span: Range<usize>, register: &mut String) {
    if span.is_empty() {
        return;
    }
    edit.record(value);
    *register = value[span.clone()].to_string();
    value.replace_range(span, "");
}

/// The byte range an operator covers when its motion lands on `target`.
///
/// Backward motions are all exclusive of where they land, which is what makes
/// `db` delete back to the start of the word and leave that character alone.
fn op_span(value: &str, caret: usize, target: usize, inclusive: bool) -> Range<usize> {
    if target >= caret {
        let end = if inclusive { next(value, target) } else { target };
        caret..end.max(caret)
    } else {
        target..caret
    }
}

impl Op {
    /// The key that types this operator, so `dd`, `cc` and `yy` are recognized
    /// without a table spelling the pairing out three times.
    fn letter(self) -> char {
        match self {
            Op::Delete => 'd',
            Op::Change => 'c',
            Op::Yank => 'y',
        }
    }
}

impl FindKind {
    /// What `,` searches: the same character the other way.
    fn reversed(self) -> Self {
        Self { forward: !self.forward, ..self }
    }
}

fn find_kind(c: char) -> FindKind {
    FindKind { forward: matches!(c, 'f' | 't'), till: matches!(c, 't' | 'T') }
}

/// Where a motion that needs nothing but its own key lands, and whether an
/// operator using it covers the character it lands on.
///
/// The single table both the plain motions and the operators read, so `w` and
/// `dw` cannot disagree about where a word ends. `None` means "not a motion" —
/// the caller decides whether that is a key to swallow or an operator to
/// cancel.
fn simple_motion(value: &str, caret: usize, c: char) -> Option<(usize, bool)> {
    let target = match c {
        // Clamped to the line: vim's `h` and `l` never cross one, and a caret
        // left sitting on a `\n` is not a position normal mode has.
        'h' => prev(value, caret).max(line_start(value, caret)),
        'l' => next(value, caret).min(line_end(value, caret)),
        'w' => word_forward(value, caret, Words::Small),
        'W' => word_forward(value, caret, Words::Big),
        'b' => word_back(value, caret, Words::Small),
        'B' => word_back(value, caret, Words::Big),
        'e' => return Some((word_end(value, caret, Words::Small), true)),
        'E' => return Some((word_end(value, caret, Words::Big), true)),
        // Measured against the line the caret is on, not the whole value. A
        // field with no newline in it is one line, so this is the old
        // behaviour everywhere except the body.
        '0' => line_start(value, caret),
        // `_` is vim's own spelling of `^` for the current line, and the one
        // people reach for first; both are bound because both are muscle
        // memory.
        '^' | '_' => first_non_blank(value, caret),
        // Exclusive of the offset it names, which is one *past* the last
        // character — so `d$` takes the rest of the line, and `$` alone lands
        // on its last character once `rest_on_char` has pulled it back.
        '$' => line_end(value, caret),
        _ => return None,
    };
    Some((target, false))
}

/// Where `f`/`F`/`t`/`T` land, searching for `target` within the caret's line.
///
/// `None` when the character is not on the line, which leaves the caret alone
/// and cancels any operator waiting on it. That is vim's behaviour and the one
/// that matters here: `dfx` with no `x` on the line must delete nothing at all.
fn find_motion(value: &str, caret: usize, kind: FindKind, target: char) -> Option<(usize, bool)> {
    let (start, end) = (line_start(value, caret), line_end(value, caret));
    if kind.forward {
        // The search starts after the caret so that `fx` twice moves twice, and
        // after the character following it for `t`, which would otherwise never
        // leave a caret already sitting one short of its target.
        let mut from = next(value, caret).min(end);
        if kind.till {
            from = next(value, from).min(end);
        }
        let at = value[from..end].find(target).map(|i| from + i)?;
        Some((if kind.till { prev(value, at) } else { at }, true))
    } else {
        let at = value[start..caret].rfind(target).map(|i| start + i)?;
        Some((if kind.till { next(value, at) } else { at }, false))
    }
}

/// The case-swapped form of one character. `char::to_uppercase` yields more
/// than one character for a few of them, which is why this yields an iterator
/// rather than a `char`.
fn swap_case(c: char) -> impl Iterator<Item = char> {
    let (upper, lower) = if c.is_lowercase() {
        (Some(c.to_uppercase()), None)
    } else {
        (None, Some(c.to_lowercase()))
    };
    upper.into_iter().flatten().chain(lower.into_iter().flatten())
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
/// leaves it. A no-op in insert mode and on an empty line.
///
/// The newline itself counts as the end: normal mode rests *on* a character,
/// and `\n` is not one you can sit on — a caret there would draw the block
/// cursor in the column past the last character, which is the position insert
/// mode owns.
fn rest_on_char(value: &str, edit: &mut Edit) {
    if edit.insert {
        return;
    }
    let at_line_end = edit.caret == value.len() || value[edit.caret..].starts_with('\n');
    if at_line_end && edit.caret > line_start(value, edit.caret) {
        edit.caret = prev(value, edit.caret);
    }
}

/// The first non-blank of the caret's line, or its start when the line is all
/// blanks. Blanks here are spaces and tabs, never the newline that ends the
/// line — `^` on a blank line must not walk onto the next one.
fn first_non_blank(value: &str, caret: usize) -> usize {
    let start = line_start(value, caret);
    let end = line_end(value, caret);
    value[start..end]
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map_or(start, |(i, _)| start + i)
}

// ─── Line arithmetic ──────────────────────────────────────────────────────────
//
// Only the body field ever holds a newline. These are written so that a value
// without one is a single line running from 0 to `len()`, which is what keeps
// every other field's behaviour identical to what it was before.

/// The offset just after the newline preceding `caret`, or 0.
fn line_start(value: &str, caret: usize) -> usize {
    value[..caret].rfind('\n').map_or(0, |i| i + 1)
}

/// The offset of the newline ending the caret's line, or `value.len()`.
fn line_end(value: &str, caret: usize) -> usize {
    value[caret..].find('\n').map_or(value.len(), |i| caret + i)
}

/// The caret's whole line, newline excluded — what `dd`, `cc`, `yy`, `Y` and
/// `S` all work over. The newline stays: removing it would join the line below
/// onto the one above, which is not what deleting a line means.
fn line_span(value: &str, caret: usize) -> (usize, usize) {
    (line_start(value, caret), line_end(value, caret))
}

/// The start of the last line, which is where `G` goes. `0` for a value with no
/// newline in it — every field but the body.
fn last_line_start(value: &str) -> usize {
    value.rfind('\n').map_or(0, |i| i + 1)
}

/// The same column one line down, clamped to that line's end. `None` on the
/// last line — the caller turns that into a row walk.
fn line_down(value: &str, caret: usize) -> Option<usize> {
    let end = line_end(value, caret);
    if end == value.len() {
        return None;
    }
    Some(at_column(value, end + 1, column(value, caret)))
}

/// The same column one line up. `None` on the first line.
fn line_up(value: &str, caret: usize) -> Option<usize> {
    let start = line_start(value, caret);
    if start == 0 {
        return None;
    }
    Some(at_column(value, line_start(value, start - 1), column(value, caret)))
}

/// How many characters into its line the caret is. Characters and not bytes,
/// so a line with a `é` in it does not shift the column under it.
fn column(value: &str, caret: usize) -> usize {
    value[line_start(value, caret)..caret].chars().count()
}

/// `column` characters into the line starting at `start`, or that line's end
/// when it is shorter — vim's own rule for a short line under the cursor.
fn at_column(value: &str, start: usize, column: usize) -> usize {
    let end = line_end(value, start);
    value[start..end]
        .char_indices()
        .nth(column)
        .map_or(end, |(i, _)| start + i)
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

/// `w`: past the current run, then past any whitespace.
///
/// With no next word it returns the end of the value rather than its last
/// character, which is one past where a normal-mode caret may rest —
/// `rest_on_char` pulls it back, so the motion lands where vim's does. The
/// distinction is for the operators: vim's `dw` on the last word of a line
/// takes the rest of that line, and stopping a character short would leave one
/// behind.
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
    chars.get(i).map_or(value.len(), |(off, _)| *off)
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

    /// [`apply`] with a throwaway register, for the tests that are not about
    /// `y` and `p`. The ones that are use [`apply`] directly and keep it.
    fn press(value: &mut String, edit: &mut Edit, key: KeyEvent, keys: Keys) -> Applied {
        apply(value, edit, key, keys, &mut String::new())
    }

    fn press_multiline(
        value: &mut String,
        edit: &mut Edit,
        key: KeyEvent,
        keys: Keys,
    ) -> Applied {
        apply_multiline(value, edit, key, keys, &mut String::new())
    }

    /// Types `presses` into `value` from a normal-mode caret at `caret`,
    /// keeping the register across them — what every operator, `y`/`p` and
    /// `f`-family test runs through.
    fn keys_at(value: &str, caret: usize, presses: &str) -> (String, Edit, String) {
        let mut value = value.to_string();
        let mut edit = normal_at(caret);
        let mut register = String::new();
        for c in presses.chars() {
            apply(&mut value, &mut edit, key(c), Keys::default(), &mut register);
        }
        (value, edit, register)
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
            press(&mut value, &mut edit, *k, keys);
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
        assert_eq!(press(&mut value, &mut edit, code(KeyCode::Esc), Keys::default()), Applied::Yes);
        assert!(!edit.insert);
        // Vim steps off the end when leaving insert mode.
        assert_eq!(edit.caret, 0);
        assert_eq!(press(&mut value, &mut edit, code(KeyCode::Esc), Keys::default()), Applied::Exit);
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
            press(&mut value, &mut edit, code(KeyCode::Left), Keys::default());
        }
        assert_eq!(edit.caret, 0);
        press(&mut value, &mut edit, code(KeyCode::Right), Keys::default());
        assert_eq!(edit.caret, 1);
        press(&mut value, &mut edit, code(KeyCode::Delete), Keys::default());
        assert_eq!(value, "a…b");
    }

    #[test]
    fn a_stale_caret_is_clamped_rather_than_panicking() {
        let mut value = "hi".to_string();
        let mut edit = Edit { caret: 99, insert: true, ..Edit::default() };
        press(&mut value, &mut edit, key('!'), Keys::default());
        assert_eq!(value, "hi!");

        // Mid-character carets snap back to a boundary.
        let mut value = "é".to_string();
        let mut edit = Edit { caret: 1, insert: true, ..Edit::default() };
        press(&mut value, &mut edit, key('x'), Keys::default());
        assert_eq!(value, "xé");
    }

    #[test]
    fn ctrl_keys_are_left_to_the_caller() {
        let mut value = String::new();
        let mut edit = Edit::default();
        let ctrl_h = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL);
        assert_eq!(press(&mut value, &mut edit, ctrl_h, Keys::default()), Applied::No);
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
            press(&mut value, &mut edit, k, Keys::default());
        }
        assert_eq!(value, "abcd");
        press(&mut value, &mut edit, key('u'), Keys::default());
        assert_eq!(value, "a", "one `u` should undo the whole session");
    }

    #[test]
    fn successive_changes_undo_one_at_a_time_in_reverse() {
        let mut value = "one".to_string();
        let mut edit = Edit::at_end(&value);
        // Session 1: append " two". Session 2: append " three".
        for k in [key(' '), key('t'), key('w'), key('o'), code(KeyCode::Esc)] {
            press(&mut value, &mut edit, k, Keys::default());
        }
        for k in [key('A'), key('!'), code(KeyCode::Esc)] {
            press(&mut value, &mut edit, k, Keys::default());
        }
        assert_eq!(value, "one two!");

        press(&mut value, &mut edit, key('u'), Keys::default());
        assert_eq!(value, "one two");
        press(&mut value, &mut edit, key('u'), Keys::default());
        assert_eq!(value, "one");
    }

    #[test]
    fn u_on_an_exhausted_history_leaves_the_field_alone() {
        let mut value = "abc".to_string();
        let mut edit = normal_at(0);
        for _ in 0..5 {
            press(&mut value, &mut edit, key('u'), Keys::default());
        }
        assert_eq!(value, "abc");
    }

    #[test]
    fn normal_mode_edits_are_each_their_own_undo_step() {
        let mut value = "abcd".to_string();
        let mut edit = normal_at(0);
        press(&mut value, &mut edit, key('x'), Keys::default());
        press(&mut value, &mut edit, key('x'), Keys::default());
        assert_eq!(value, "cd");
        press(&mut value, &mut edit, key('u'), Keys::default());
        assert_eq!(value, "bcd");
        press(&mut value, &mut edit, key('u'), Keys::default());
        assert_eq!(value, "abcd");
    }

    #[test]
    fn undo_restores_the_caret_with_the_text() {
        let mut value = "abc".to_string();
        let mut edit = normal_at(2);
        press(&mut value, &mut edit, key('D'), Keys::default());
        assert_eq!(value, "ab");
        press(&mut value, &mut edit, key('u'), Keys::default());
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
            press(&mut value, &mut edit, k, Keys::default());
        }
        assert_eq!(value, "abc");
        press(&mut value, &mut edit, key('u'), Keys::default());
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
        press(&mut value, &mut edit, key('j'), jk());
        // Backdate the arming past `timeoutlen`: a pause then `k` is a literal.
        edit.pending = edit
            .pending
            .map(|(c, at)| (c, at - SEQUENCE_TIMEOUT - Duration::from_millis(1)));
        press(&mut value, &mut edit, key('k'), jk());
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
        press(&mut value, &mut edit, key('j'), jk());
        insert_str(&mut value, &mut edit, "X");
        press(&mut value, &mut edit, key('k'), jk());
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
        for (walk, expected) in [('j', Applied::FocusNext), ('k', Applied::FocusPrev)] {
            let mut value = "https://x".to_string();
            let mut edit = normal_at(4);
            assert_eq!(press(&mut value, &mut edit, key(walk), Keys::default()), expected);
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

    // ─── Multi-line fields (the request body) ─────────────────────────────────

    fn run_multiline(value: &str, edit: Edit, presses: &[KeyEvent]) -> (String, Edit) {
        let mut value = value.to_string();
        let mut edit = edit;
        for k in presses {
            press_multiline(&mut value, &mut edit, *k, Keys::default());
        }
        (value, edit)
    }

    /// The pane's whole reason for `apply_multiline`: Enter is a character, not
    /// a command, so a body pasted as keystrokes keeps its lines.
    #[test]
    fn enter_types_a_newline_in_a_multiline_field() {
        let (value, edit) = run_multiline(
            "",
            Edit::default(),
            &[key('{'), code(KeyCode::Enter), key('}')],
        );
        assert_eq!(value, "{\n}");
        assert_eq!(edit.caret, 3);
    }

    /// `j` and `k` are a line motion while there is a line to reach, and the
    /// form's row walk only at the edges — otherwise `j` in a twenty-line body
    /// would leave the field on the first press.
    #[test]
    fn j_and_k_move_by_line_before_they_walk_rows() {
        let body = "one\ntwo\nthree";
        let mut value = body.to_string();
        let mut edit = normal_at(1);

        assert_eq!(press_multiline(&mut value, &mut edit, key('j'), Keys::default()), Applied::Yes);
        assert_eq!(edit.caret, 5);
        assert_eq!(press_multiline(&mut value, &mut edit, key('j'), Keys::default()), Applied::Yes);
        assert_eq!(edit.caret, 9);
        // Off the bottom: now it is the pane's business.
        assert_eq!(press_multiline(&mut value, &mut edit, key('j'), Keys::default()), Applied::FocusNext);

        let mut edit = normal_at(1);
        assert_eq!(press_multiline(&mut value, &mut edit, key('k'), Keys::default()), Applied::FocusPrev);
        assert_eq!(value, body);
    }

    /// A shorter line under the cursor takes its end, as vim does.
    #[test]
    fn a_vertical_move_clamps_to_a_shorter_line() {
        let (_, edit) = run_multiline("longer\nab\nlonger", normal_at(4), &[key('j')]);
        // "ab" is two characters; normal mode rests on the last of them.
        assert_eq!(edit.caret, 8);
    }

    /// `0`, `^` and `$` are about a line, so in a body they are about *the*
    /// line — and in every single-line field they are unchanged.
    #[test]
    fn line_motions_stay_on_their_line() {
        let body = "  first\nsecond";
        let (_, edit) = run_multiline(body, normal_at(10), &[key('0')]);
        assert_eq!(edit.caret, 8);
        let (_, edit) = run_multiline(body, normal_at(4), &[key('$')]);
        assert_eq!(edit.caret, 6);
        let (_, edit) = run_multiline(body, normal_at(4), &[key('^')]);
        assert_eq!(edit.caret, 2);

        // The single-line case: one line running the whole value.
        let (_, edit) = run("  abc", normal_at(4), &[key('^')]);
        assert_eq!(edit.caret, 2);
        let (_, edit) = run("  abc", normal_at(0), &[key('$')]);
        assert_eq!(edit.caret, 4);
    }

    /// `D` and `S` clear a line, not the rest of the body. Getting this wrong
    /// is not cosmetic: `D` on line one of a JSON body would delete the body.
    #[test]
    fn d_and_s_stop_at_the_end_of_the_line() {
        let (value, _) = run_multiline("one\ntwo\nthree", normal_at(5), &[key('D')]);
        assert_eq!(value, "one\nt\nthree");

        let (value, edit) = run_multiline("one\ntwo\nthree", normal_at(5), &[key('S')]);
        assert_eq!(value, "one\n\nthree");
        assert_eq!(edit.caret, 4);
        assert!(edit.insert);
    }

    /// Normal mode rests *on* a character, and a newline is not one to sit on.
    #[test]
    fn the_caret_never_rests_on_a_newline() {
        let (_, edit) = run_multiline("ab\ncd", normal_at(0), &[key('l'), key('l')]);
        assert_eq!(edit.caret, 1);

        // Leaving insert mode at the end of a line steps back the same way.
        let (_, edit) = run_multiline("ab\ncd", Edit { caret: 2, insert: true, ..Edit::default() }, &[code(KeyCode::Esc)]);
        assert_eq!(edit.caret, 1);
    }

    /// An empty line is a place the caret can be, so `Enter` at the end of a
    /// body has somewhere to land.
    #[test]
    fn the_caret_can_sit_on_an_empty_line() {
        let (value, edit) = run_multiline("a", Edit::at_end("a"), &[code(KeyCode::Enter)]);
        assert_eq!(value, "a\n");
        assert_eq!(edit.caret, 2);
    }

    /// Undo treats a whole typed body as one change, exactly as it does a URL.
    #[test]
    fn undo_walks_back_a_multiline_insert_session() {
        let (value, edit) = run_multiline(
            "",
            Edit::default(),
            &[key('a'), code(KeyCode::Enter), key('b'), code(KeyCode::Esc), key('u')],
        );
        assert_eq!(value, "");
        assert_eq!(edit.caret, 0);
    }

    // ─── Line motions ─────────────────────────────────────────────────────────

    /// `_` is the one the issue asked for by name, and it has to agree with
    /// `^` — both are vim's "first non-blank of this line".
    #[test]
    fn underscore_and_caret_are_the_same_motion() {
        for motion in ["_", "^"] {
            let (_, edit, _) = keys_at("   https://x", 8, motion);
            assert_eq!(edit.caret, 3, "{motion}");
        }
    }

    #[test]
    fn dollar_lands_on_the_last_character_and_zero_on_the_first() {
        let (_, edit, _) = keys_at("abcdef", 2, "$");
        assert_eq!(edit.caret, 5);
        let (_, edit, _) = keys_at("abcdef", 2, "0");
        assert_eq!(edit.caret, 0);
    }

    /// `gg` and `G` are the first and last lines' first non-blank. In a
    /// single-line field that is the same place, exactly as in vim.
    #[test]
    fn gg_and_g_walk_to_the_first_and_last_lines() {
        let mut value = "  one\n  two\n  three".to_string();
        let mut edit = normal_at(8);
        let mut register = String::new();
        for c in "gg".chars() {
            apply(&mut value, &mut edit, key(c), Keys::default(), &mut register);
        }
        assert_eq!(edit.caret, 2);
        apply(&mut value, &mut edit, key('G'), Keys::default(), &mut register);
        assert_eq!(edit.caret, 14);
    }

    /// `h` at the start of a line and `l` at its end stay put rather than
    /// stepping onto the newline, which is not a position normal mode has.
    #[test]
    fn h_and_l_do_not_cross_a_line_break() {
        let (_, edit, _) = keys_at("ab\ncd", 3, "h");
        assert_eq!(edit.caret, 3);
        let (_, edit, _) = keys_at("ab\ncd", 1, "l");
        assert_eq!(edit.caret, 1);
    }

    // ─── Character search ─────────────────────────────────────────────────────

    #[test]
    fn f_and_t_search_forward_and_stop_differently() {
        let (_, edit, _) = keys_at("Bearer abc.def", 0, "f.");
        assert_eq!(edit.caret, 10);
        let (_, edit, _) = keys_at("Bearer abc.def", 0, "t.");
        assert_eq!(edit.caret, 9);
    }

    #[test]
    fn capital_f_searches_backwards() {
        let (_, edit, _) = keys_at("a.b.c", 4, "F.");
        assert_eq!(edit.caret, 3);
        let (_, edit, _) = keys_at("a.b.c", 4, "T.");
        assert_eq!(edit.caret, 4);
    }

    /// A character that is not on the line leaves the caret alone — and an
    /// operator waiting on that search does nothing at all.
    #[test]
    fn a_search_that_finds_nothing_changes_nothing() {
        let (value, edit, _) = keys_at("abc", 0, "fz");
        assert_eq!((value.as_str(), edit.caret), ("abc", 0));
        let (value, edit, _) = keys_at("abc", 0, "dfz");
        assert_eq!((value.as_str(), edit.caret), ("abc", 0));
    }

    /// The search never leaves the caret's line, so `f` cannot jump into the
    /// body's next line the way a whole-value search would.
    #[test]
    fn a_search_stays_on_the_caret_s_line() {
        let (_, edit, _) = keys_at("abc\nx.y", 0, "f.");
        assert_eq!(edit.caret, 0);
    }

    #[test]
    fn semicolon_repeats_a_search_and_comma_reverses_it() {
        let (_, edit, _) = keys_at("a.b.c.d", 0, "f;");
        // `f` then `;` — the `;` is the character being searched for here, so
        // this is a search for a semicolon that finds nothing.
        assert_eq!(edit.caret, 0);

        let (_, edit, _) = keys_at("a.b.c.d", 0, "f.;");
        assert_eq!(edit.caret, 3);
        let (_, edit, _) = keys_at("a.b.c.d", 0, "f.;;,");
        assert_eq!(edit.caret, 3);
    }

    // ─── Operators ────────────────────────────────────────────────────────────

    #[test]
    fn dw_deletes_a_word_and_the_space_after_it() {
        let (value, edit, register) = keys_at("one two three", 0, "dw");
        assert_eq!(value, "two three");
        assert_eq!(edit.caret, 0);
        assert_eq!(register, "one ");
    }

    /// Vim's one irregular pair: `cw` on a non-blank behaves like `ce`, so the
    /// space after the word survives.
    #[test]
    fn cw_changes_the_word_but_not_the_space_after_it() {
        let (value, edit, _) = keys_at("one two", 0, "cw");
        assert_eq!(value, " two");
        assert!(edit.insert);
        assert_eq!(edit.caret, 0);
    }

    #[test]
    fn db_deletes_back_to_the_start_of_the_word() {
        let (value, edit, _) = keys_at("one two three", 8, "db");
        assert_eq!(value, "one three");
        assert_eq!(edit.caret, 4);
    }

    #[test]
    fn d_dollar_takes_the_rest_of_the_line_and_d0_the_start_of_it() {
        let (value, _, register) = keys_at("abcdef", 3, "d$");
        assert_eq!(value, "abc");
        assert_eq!(register, "def");
        let (value, edit, _) = keys_at("abcdef", 3, "d0");
        assert_eq!(value, "def");
        assert_eq!(edit.caret, 0);
    }

    /// `de` is inclusive of the character it lands on, which is what makes it
    /// different from `dw` at the end of a line.
    #[test]
    fn de_includes_the_last_character_of_the_word() {
        let (value, _, _) = keys_at("one two", 0, "de");
        assert_eq!(value, " two");
    }

    #[test]
    fn df_deletes_up_to_and_including_its_target() {
        let (value, _, _) = keys_at("key=value;rest", 0, "df;");
        assert_eq!(value, "rest");
        let (value, _, _) = keys_at("key=value;rest", 0, "dt;");
        assert_eq!(value, ";rest");
    }

    /// The doubled operators take the line's text and leave the newline, so a
    /// `dd` in the body does not join the line below onto the one above.
    #[test]
    fn dd_empties_the_line_without_removing_it() {
        let (value, edit, register) = keys_at("one\ntwo\nthree", 5, "dd");
        assert_eq!(value, "one\n\nthree");
        assert_eq!(edit.caret, 4);
        assert_eq!(register, "two");
    }

    #[test]
    fn cc_empties_the_line_and_opens_insert_mode() {
        let (value, edit, _) = keys_at("one\ntwo", 5, "cc");
        assert_eq!(value, "one\n");
        assert!(edit.insert);
        assert_eq!(edit.caret, 4);
    }

    /// A yank never changes the text, and leaves the caret at the start of what
    /// it took.
    #[test]
    fn yank_fills_the_register_and_leaves_the_value_alone() {
        let (value, edit, register) = keys_at("one two", 4, "yy");
        assert_eq!(value, "one two");
        assert_eq!(register, "one two");
        assert_eq!(edit.caret, 0);

        let (value, _, register) = keys_at("one two", 4, "yw");
        assert_eq!(value, "one two");
        assert_eq!(register, "two");
    }

    #[test]
    fn capital_y_yanks_the_line_without_moving_the_caret() {
        let (value, edit, register) = keys_at("one\ntwo", 5, "Y");
        assert_eq!(value, "one\ntwo");
        assert_eq!(register, "two");
        assert_eq!(edit.caret, 5);
    }

    /// A key that is not a motion cancels the operator rather than being read
    /// as a fresh command — a mistyped `dq` must not delete on the next key.
    #[test]
    fn an_operator_waiting_on_a_motion_is_cancelled_by_anything_else() {
        let (value, edit, _) = keys_at("one two", 0, "dq");
        assert_eq!(value, "one two");
        assert_eq!(edit.caret, 0);
        // The `w` that follows is a plain motion, not the tail of the `d`.
        let (value, edit, _) = keys_at("one two", 0, "dqw");
        assert_eq!(value, "one two");
        assert_eq!(edit.caret, 4);
    }

    /// Anything that does not reach the normal-mode table drops the operator,
    /// which is what `apply` taking `pending_cmd` on the way in buys.
    #[test]
    fn an_arrow_key_cancels_a_waiting_operator() {
        let mut value = "one two".to_string();
        let mut edit = normal_at(0);
        let mut register = String::new();
        for k in [key('d'), code(KeyCode::Right), key('w')] {
            apply(&mut value, &mut edit, k, Keys::default(), &mut register);
        }
        assert_eq!(value, "one two");
    }

    #[test]
    fn an_operator_is_one_undo_step() {
        let (value, _, _) = keys_at("one two three", 0, "dwu");
        assert_eq!(value, "one two three");
    }

    // ─── Register, replace, case ──────────────────────────────────────────────

    /// The register is charwise, so `p` puts after the character under the
    /// caret and leaves the caret on the last character put.
    #[test]
    fn p_puts_after_the_caret_and_capital_p_before_it() {
        let (value, edit, _) = keys_at("ab", 0, "ylp");
        assert_eq!(value, "aab");
        assert_eq!(edit.caret, 1);
        let (value, edit, _) = keys_at("ab", 0, "ylP");
        assert_eq!(value, "aab");
        assert_eq!(edit.caret, 0);
    }

    #[test]
    fn p_with_an_empty_register_does_nothing() {
        let (value, edit, _) = keys_at("ab", 0, "p");
        assert_eq!(value, "ab");
        assert_eq!(edit.caret, 0);
    }

    /// The register outlives the field: `apply` takes it by reference, so the
    /// caller can hand the same one to the next field's edit.
    #[test]
    fn the_register_carries_between_two_fields() {
        let mut register = String::new();
        let mut from = "secret".to_string();
        let mut edit = normal_at(0);
        for c in "y$".chars() {
            apply(&mut from, &mut edit, key(c), Keys::default(), &mut register);
        }
        let mut to = "x".to_string();
        let mut edit = normal_at(0);
        apply(&mut to, &mut edit, key('p'), Keys::default(), &mut register);
        assert_eq!(from, "secret");
        assert_eq!(to, "xsecret");
    }

    #[test]
    fn x_and_d_fill_the_register_too() {
        let (_, _, register) = keys_at("abc", 0, "x");
        assert_eq!(register, "a");
        let (_, _, register) = keys_at("abc", 0, "D");
        assert_eq!(register, "abc");
    }

    #[test]
    fn r_writes_one_character_without_leaving_normal_mode() {
        let (value, edit, _) = keys_at("cat", 0, "rb");
        assert_eq!(value, "bat");
        assert_eq!(edit.caret, 0);
        assert!(!edit.insert);
    }

    /// `r` must not write over the newline ending a line — joining two lines is
    /// not what `r` means.
    #[test]
    fn r_on_an_empty_line_writes_nothing() {
        let (value, _, _) = keys_at("a\n\nb", 2, "rx");
        assert_eq!(value, "a\n\nb");
    }

    #[test]
    fn tilde_swaps_the_case_under_the_caret_and_steps_on() {
        let (value, edit, _) = keys_at("abc", 0, "~~");
        assert_eq!(value, "ABc");
        assert_eq!(edit.caret, 2);
    }

    #[test]
    fn r_and_tilde_are_each_one_undo_step() {
        let (value, _, _) = keys_at("cat", 0, "rbu");
        assert_eq!(value, "cat");
        let (value, _, _) = keys_at("cat", 0, "~u");
        assert_eq!(value, "cat");
    }

    /// None of these are commands in insert mode: they are letters someone is
    /// typing into a URL.
    #[test]
    fn the_new_commands_are_plain_characters_in_insert_mode() {
        let (value, _) = run("", Edit::default(), &"dcyrgfp~".chars().map(key).collect::<Vec<_>>());
        assert_eq!(value, "dcyrgfp~");
    }

    /// `j` in a body still moves by line rather than completing an operator —
    /// `apply_multiline` owns that key, so it drops the operator instead.
    #[test]
    fn a_body_line_walk_cancels_a_waiting_operator() {
        let mut value = "one\ntwo".to_string();
        let mut edit = normal_at(0);
        let mut register = String::new();
        for k in [key('d'), key('j')] {
            apply_multiline(&mut value, &mut edit, k, Keys::default(), &mut register);
        }
        assert_eq!(value, "one\ntwo");
        assert_eq!(edit.caret, 4);
    }
}
