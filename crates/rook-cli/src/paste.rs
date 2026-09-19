//! Telling a paste from typing when the reader of the terminal cannot.
//!
//! Bracketed paste is the answer on unix: the terminal wraps what was pasted
//! in `ESC [ 200 ~` … `ESC [ 201 ~`, crossterm reads the pair and hands the
//! whole of it over as one `Event::Paste`, newlines included. On Windows
//! crossterm reads console input records instead, and that reader has never
//! heard of the bracket: what arrives is one key event per character, with
//! every pasted newline as the Enter key — so the first line of a pasted
//! paragraph was sent as a prompt and the rest chased it, the exact fault
//! bracketed paste had been enabled against, on the one platform where
//! enabling it changes nothing the reader can see. The terminal may still be
//! bracketing: Windows Terminal honours the mode and sends the six characters
//! of each marker as six keystrokes — the first of them an Esc, which on an
//! empty box quits the window — and a console host that does not know the
//! mode sends nothing at all. Both shapes end here.
//!
//! What tells a paste from typing without the bracket is time. Console input
//! is queued as it is written, and a paste is written whole, so its keys are
//! already queued behind one another when the first is read; a hand puts tens
//! of milliseconds between keys and never ten. So a key with nothing queued
//! behind it is typing and costs nothing to answer, and a run of keys that
//! arrived together is read to its end and looked at as a whole: a marker at
//! the front makes it a paste outright, and without one, a newline with text
//! after it does — a paragraph is what nobody types inside ten milliseconds.
//! Everything else is handed back as the keys it was, in order, so typing that
//! queued up behind a slow redraw is still typing, and an Enter at the end of
//! it still sends. A single line pasted with its newline is sent too, which is
//! what a shell does with one.

use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};

/// Keys closer together than this arrived from a paste. A hand puts tens of
/// milliseconds between keys and machine input microseconds; a console host
/// handing a large paste over in pieces lands between, and ten is over the
/// gaps of that kind and under any two keystrokes a person produces.
const TOGETHER: Duration = Duration::from_millis(10);

/// Inside a bracket the end marker is waited for this long. The marker has
/// already said a paste is in progress, so the question is no longer typing or
/// not but how long the host takes to hand the rest over, and a big paste
/// through a console pipe takes longer than ten milliseconds between pieces.
const BRACKETED: Duration = Duration::from_millis(250);

/// The two markers a terminal puts round a paste, as the keys they arrive as.
const OPEN: &str = "\x1b[200~";
const CLOSE: &str = "\x1b[201~";
const MARKER: usize = 6;

/// Where keys come from: the terminal, or a script of them in a test.
pub(crate) trait Keys {
    /// The next event, if one arrives within `wait`.
    fn next(&mut self, wait: Duration) -> std::io::Result<Option<Event>>;
    /// The clock the waits are measured on: the wall for the terminal, and a
    /// script's own for a script, so a test can say how long something took.
    fn now(&self) -> Instant;
}

/// The terminal crossterm is reading.
pub(crate) struct Terminal;

impl Keys for Terminal {
    fn next(&mut self, wait: Duration) -> std::io::Result<Option<Event>> {
        match event::poll(wait)? {
            true => event::read().map(Some),
            false => Ok(None),
        }
    }

    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// What a run of keys that arrived together turned out to be.
#[derive(Debug, PartialEq)]
pub(crate) enum Burst {
    /// Typing: each key is handled as itself, in this order.
    Keys(Vec<KeyEvent>),
    /// A paste: text with its newlines, for the box and never for sending.
    Paste(String),
}

/// A burst read from the terminal, and whatever else arrived during it.
pub(crate) struct Gathered {
    pub(crate) burst: Burst,
    /// Events that are not keys — the wheel, a resize — that arrived while the
    /// keys were being read and are still owed their turn, in order.
    pub(crate) then: Vec<Event>,
}

/// Read the keys queued behind `first`, and any that follow within the time a
/// paste puts between keys, and say what the run was.
pub(crate) fn gather(first: KeyEvent, source: &mut impl Keys) -> std::io::Result<Gathered> {
    let mut keys = vec![first];
    let mut then = Vec::new();
    // Nothing behind it is the ordinary case, and typing must not pay for a
    // paste it is not: the first look is at what is already queued, not a wait.
    let mut until = source.now();
    loop {
        let wait = until.saturating_duration_since(source.now());
        let Some(event) = source.next(wait)? else { break };
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                keys.push(key);
                if opens_bracket(&keys) && closes_bracket(&keys) {
                    break;
                }
                // A key renews the patience, and only a key. The wheel, or a
                // mouse moving over the window, reports every few milliseconds,
                // and if each report renewed it a key pressed while the mouse
                // was moving would wait for the mouse to stop.
                let patience = match opens_bracket(&keys) {
                    true => BRACKETED,
                    false => TOGETHER,
                };
                until = source.now() + patience;
            }
            // A key coming back up is not a key going down, and a paste arrives
            // as pairs of both.
            Event::Key(_) => {}
            other => then.push(other),
        }
    }
    Ok(Gathered { burst: classify(keys), then })
}

/// The whole run, judged: a bracket is the terminal's word for a paste, and a
/// newline with text after it is one without the word.
fn classify(keys: Vec<KeyEvent>) -> Burst {
    if opens_bracket(&keys) {
        let end = match closes_bracket(&keys) {
            true => keys.len() - MARKER,
            false => keys.len(),
        };
        return Burst::Paste(text_of(&keys[MARKER..end]));
    }
    match keys.len() > 1 && keys.iter().all(is_text) && has_a_line_after_a_newline(&keys) {
        true => Burst::Paste(text_of(&keys)),
        false => Burst::Keys(keys),
    }
}

fn opens_bracket(keys: &[KeyEvent]) -> bool {
    keys.len() >= MARKER && spells(&keys[..MARKER], OPEN)
}

/// Closed only once there is room for both markers: a paste of nothing is
/// still the two of them, and the opening one must not be read as closing.
fn closes_bracket(keys: &[KeyEvent]) -> bool {
    keys.len() >= 2 * MARKER && spells(&keys[keys.len() - MARKER..], CLOSE)
}

fn spells(keys: &[KeyEvent], marker: &str) -> bool {
    keys.len() == marker.chars().count()
        && keys.iter().zip(marker.chars()).all(|(key, c)| match c {
            '\x1b' => key.code == KeyCode::Esc,
            c => key.code == KeyCode::Char(c),
        })
}

/// Modifiers are not looked at: a pasted capital arrives with Shift held, and
/// a character that needs AltGr on the keyboard it was typed into arrives with
/// Control and Alt, because the console host synthesises the keystroke that
/// would have produced it.
fn is_text(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(_) | KeyCode::Enter | KeyCode::Tab)
}

fn is_newline(key: &KeyEvent) -> bool {
    matches!(key.code, KeyCode::Enter | KeyCode::Char('\r') | KeyCode::Char('\n'))
}

/// A newline somewhere with text on the far side of it. Not merely a newline
/// that is not last: two Enters tapped together are two Enters, and a paste of
/// nothing but newlines has nothing in it worth keeping from being sent.
fn has_a_line_after_a_newline(keys: &[KeyEvent]) -> bool {
    keys.iter()
        .position(is_newline)
        .is_some_and(|at| keys[at..].iter().any(|key| is_text(key) && !is_newline(key)))
}

/// The keys as the text they spell. The Enter key is a `\r`, and a host that
/// hands over the `\n` behind it as well meant one line by the pair.
fn text_of(keys: &[KeyEvent]) -> String {
    let mut text = String::with_capacity(keys.len());
    let mut after_return = false;
    for key in keys {
        match key.code {
            KeyCode::Char('\n') if after_return => {}
            KeyCode::Enter | KeyCode::Char('\r') | KeyCode::Char('\n') => text.push('\n'),
            KeyCode::Tab => text.push('\t'),
            KeyCode::Char(c) => text.push(c),
            // Nothing else is text, and a paste has no arrows in it.
            _ => {}
        }
        after_return = matches!(key.code, KeyCode::Enter | KeyCode::Char('\r'));
    }
    text
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use ratatui::crossterm::event::KeyModifiers;

    use super::*;

    /// Events arriving on a schedule: each some time after the one before,
    /// which is what the reader has to judge by.
    struct Script {
        events: VecDeque<(Duration, Event)>,
        asked: Vec<Duration>,
        /// A clock of its own, moved by what each event was scheduled after
        /// and by a wait that produced nothing.
        started: Instant,
        elapsed: Duration,
    }

    impl Keys for Script {
        fn next(&mut self, wait: Duration) -> std::io::Result<Option<Event>> {
            self.asked.push(wait);
            match self.events.front() {
                Some((after, _)) if *after <= wait => {
                    self.elapsed += *after;
                    Ok(self.events.pop_front().map(|(_, event)| event))
                }
                _ => {
                    self.elapsed += wait;
                    Ok(None)
                }
            }
        }

        fn now(&self) -> Instant {
            self.started + self.elapsed
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// The keys a console host makes of `text`, all queued at once.
    fn keys(text: &str) -> Vec<(Duration, Event)> {
        text.chars()
            .map(|c| match c {
                '\n' => KeyCode::Enter,
                '\t' => KeyCode::Tab,
                '\x1b' => KeyCode::Esc,
                c => KeyCode::Char(c),
            })
            .map(|code| (Duration::ZERO, Event::Key(key(code))))
            .collect()
    }

    /// Read `events` as the terminal would deliver them, the first as the key
    /// the loop already has in hand.
    fn read(mut events: Vec<(Duration, Event)>) -> (Gathered, Script) {
        let Event::Key(first) = events.remove(0).1 else { unreachable!("a script starts with a key") };
        let mut script = Script {
            events: events.into(),
            asked: Vec::new(),
            started: Instant::now(),
            elapsed: Duration::ZERO,
        };
        let gathered = gather(first, &mut script).expect("a script does not fail");
        (gathered, script)
    }

    fn typed(text: &str) -> Vec<KeyEvent> {
        keys(text)
            .into_iter()
            .map(|(_, event)| match event {
                Event::Key(key) => key,
                other => unreachable!("{other:?} is not a key"),
            })
            .collect()
    }

    #[test]
    fn a_key_with_nothing_behind_it_is_typing_and_waits_for_nothing() {
        let (got, script) = read(keys("a"));
        assert_eq!(got.burst, Burst::Keys(typed("a")));
        assert_eq!(script.asked, vec![Duration::ZERO], "typing paid a wait: {:?}", script.asked);
    }

    /// The complaint: a paragraph pasted into the box went out one line at a
    /// time, the first as a prompt and the rest chasing it.
    #[test]
    fn a_pasted_paragraph_arriving_as_keystrokes_is_one_paste_and_not_a_prompt_per_line() {
        let (got, _) = read(keys("first line\nsecond line\n\tthird"));
        assert_eq!(got.burst, Burst::Paste("first line\nsecond line\n\tthird".into()));
    }

    /// A slow redraw queues a hand's keys behind one another, and they still
    /// have to be the keys they were: the word typed and the Enter sending it.
    #[test]
    fn a_word_and_its_enter_queued_behind_a_redraw_are_still_typing() {
        let (got, _) = read(keys("hi\n"));
        assert_eq!(got.burst, Burst::Keys(typed("hi\n")), "a typed word and its Enter were read as a paste");
    }

    #[test]
    fn an_enter_tapped_twice_is_two_enters_and_not_a_paste() {
        let (got, _) = read(keys("\n\n"));
        assert_eq!(got.burst, Burst::Keys(typed("\n\n")));
    }

    #[test]
    fn a_burst_with_an_arrow_in_it_is_typing() {
        let mut events = keys("ab\nc");
        events.insert(2, (Duration::ZERO, Event::Key(key(KeyCode::Left))));
        let (got, _) = read(events);
        assert!(matches!(got.burst, Burst::Keys(ref keys) if keys.len() == 5), "{:?}", got.burst);
    }

    /// The terminal brackets the paste and the reader hands the bracket over
    /// as six keystrokes, the first of them an Esc: read here, it is a paste
    /// with the markers taken off and its last newline kept, not an Esc that
    /// quits the window followed by `[200~` typed into the box.
    #[test]
    fn a_bracket_the_reader_does_not_know_is_read_here_and_not_typed() {
        let (got, _) = read(keys("\x1b[200~one\ntwo\n\x1b[201~"));
        assert_eq!(got.burst, Burst::Paste("one\ntwo\n".into()));
    }

    /// One line in a bracket is still a paste: the bracket says so, and the
    /// newline rule is for when nothing does.
    #[test]
    fn a_bracketed_line_is_a_paste_even_without_a_newline_in_it() {
        let (got, _) = read(keys("\x1b[200~just this\x1b[201~"));
        assert_eq!(got.burst, Burst::Paste("just this".into()));
    }

    #[test]
    fn an_esc_pressed_by_hand_is_an_esc() {
        let (got, _) = read(keys("\x1b"));
        assert_eq!(got.burst, Burst::Keys(typed("\x1b")));
    }

    /// A console host hands a big paste over in pieces. Inside a bracket the
    /// pause between pieces is waited out, because the bracket has already
    /// said what this is; outside one the same pause ends the burst, because
    /// a hundred milliseconds is a hand.
    #[test]
    fn inside_a_bracket_a_pause_the_host_takes_is_waited_out_and_outside_one_it_is_not() {
        let pause = Duration::from_millis(100);
        let mut bracketed = keys("\x1b[200~one\ntwo");
        bracketed.extend(keys("\nthree\x1b[201~"));
        bracketed[13].0 = pause;
        let (got, script) = read(bracketed);
        assert_eq!(got.burst, Burst::Paste("one\ntwo\nthree".into()));
        assert!(script.events.is_empty(), "the bracket was not read to its end: {:?}", script.events);

        let mut bare = keys("one\ntwo");
        bare.extend(keys("\nthree"));
        bare[7].0 = pause;
        let (got, script) = read(bare);
        assert_eq!(got.burst, Burst::Paste("one\ntwo".into()));
        assert_eq!(script.events.len(), 6, "a hand's pause was waited through: {:?}", script.events);
    }

    /// A bracket never closed — a host that dropped the end marker — is still
    /// the paste it had, once the patience runs out.
    #[test]
    fn a_bracket_never_closed_is_still_the_paste_it_had() {
        let (got, _) = read(keys("\x1b[200~one\ntwo"));
        assert_eq!(got.burst, Burst::Paste("one\ntwo".into()));
    }

    /// The wheel and a mouse moving over the window report every few
    /// milliseconds. Only a key renews the patience, so a key pressed while the
    /// mouse moves is handled when it is pressed and not when the mouse stops.
    #[test]
    fn a_mouse_moving_over_the_window_does_not_hold_a_key_until_it_stops() {
        let every = Duration::from_millis(3);
        let mut events = keys("ab");
        events.extend((0..3).map(|_| (every, Event::FocusGained)));
        events.extend(keys("c").into_iter().map(|(_, event)| (every, event)));
        let (got, script) = read(events);
        assert_eq!(got.burst, Burst::Keys(typed("ab")), "a key twelve milliseconds on was gathered in");
        assert_eq!(got.then.len(), 3, "the reports were not kept: {:?}", got.then);
        assert_eq!(script.events.len(), 1, "the late key was taken: {:?}", script.events);
    }

    #[test]
    fn the_wheel_turning_during_a_paste_is_kept_for_after_it() {
        let mut events = keys("one\ntwo");
        events.insert(4, (Duration::ZERO, Event::Resize(80, 24)));
        let (got, _) = read(events);
        assert_eq!(got.burst, Burst::Paste("one\ntwo".into()));
        assert_eq!(got.then, vec![Event::Resize(80, 24)]);
    }

    /// The Enter key is the `\r`; a host that also hands over the `\n` behind
    /// it means one line by the two, not a blank one between every pair.
    #[test]
    fn a_return_with_a_line_feed_behind_it_is_one_newline() {
        let mut events = keys("one\n");
        events.push((Duration::ZERO, Event::Key(key(KeyCode::Char('\n')))));
        events.extend(keys("two"));
        let (got, _) = read(events);
        assert_eq!(got.burst, Burst::Paste("one\ntwo".into()));
    }
}
