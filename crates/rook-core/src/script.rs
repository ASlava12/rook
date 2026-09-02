//! Catching a reply that changed writing system partway through.
//!
//! Multilingual models — small and quantised ones especially — sometimes finish
//! a sentence in a script nobody used: a Russian answer with a Han word in the
//! middle of it. The text cannot be repaired from here, because only the model
//! knows what it meant, so this notices and asks for the answer again.
//!
//! Mixing scripts is usually correct — an identifier, a unit, a file name, a
//! quoted file — so the question is never "is there more than one script here"
//! but "is there one this conversation has not used".

use std::collections::BTreeSet;

/// Writing systems, grouped as one where they are written as one.
///
/// Japanese runs Han and both kana together in ordinary prose and Korean mixes
/// Hangul with Han, so telling those apart would report every Japanese sentence
/// as a slip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Script {
    Latin,
    Greek,
    Cyrillic,
    Hebrew,
    Arabic,
    Devanagari,
    Cjk,
}

impl Script {
    pub fn name(self) -> &'static str {
        match self {
            Script::Latin => "Latin",
            Script::Greek => "Greek",
            Script::Cyrillic => "Cyrillic",
            Script::Hebrew => "Hebrew",
            Script::Arabic => "Arabic",
            Script::Devanagari => "Devanagari",
            Script::Cjk => "Han, kana or Hangul",
        }
    }
}

fn script_of(c: char) -> Option<Script> {
    match c as u32 {
        0x41..=0x5a | 0x61..=0x7a | 0xc0..=0x24f => Some(Script::Latin),
        0x370..=0x3ff | 0x1f00..=0x1fff => Some(Script::Greek),
        0x400..=0x52f => Some(Script::Cyrillic),
        0x590..=0x5ff => Some(Script::Hebrew),
        0x600..=0x6ff | 0x750..=0x77f => Some(Script::Arabic),
        0x900..=0x97f => Some(Script::Devanagari),
        0x3040..=0x30ff | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xac00..=0xd7af | 0xf900..=0xfaff => {
            Some(Script::Cjk)
        }
        _ => None,
    }
}

/// The scripts `text` is written in, ignoring anything in backticks.
///
/// Code is exempt because it is not the model's own voice: a file it read, a
/// command it ran, an identifier it quoted. A CJK string literal in a source
/// file is not the model changing language.
pub fn scripts(text: &str) -> BTreeSet<Script> {
    let mut found = BTreeSet::new();
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        found.extend(rest[..open].chars().filter_map(script_of));
        let after = &rest[open..];
        let fence = "`".repeat(after.chars().take_while(|c| *c == '`').count());
        rest = match after[fence.len()..].find(&fence) {
            Some(close) => &after[fence.len() + close + fence.len()..],
            // Unclosed, so everything after it is inside the span.
            None => "",
        };
    }
    found.extend(rest.chars().filter_map(script_of));
    found
}

/// The script a reply used that nothing else in the conversation did.
///
/// Latin is never a slip: identifiers, units, brand names and command lines are
/// Latin in prose of every script, and a model writing `read_file` in a Russian
/// sentence is doing its job.
pub fn slipped(reply: &BTreeSet<Script>, known: &BTreeSet<Script>) -> Option<Script> {
    reply.iter().copied().find(|s| *s != Script::Latin && !known.contains(s))
}

/// What the model is told about it.
///
/// The whole answer again rather than a correction: handed its own text and
/// asked to change one word, a model tends to hand the same text back.
pub fn say_again(slipped: Script, known: &BTreeSet<Script>) -> String {
    let named: Vec<&str> = known.iter().map(|s| s.name()).collect();
    format!(
        "That reply used {} and nothing else here is written in it — everything so far is {}. \
         Say the whole answer again, all in the writing system this conversation is in. Code, \
         identifiers and file names stay exactly as they are.",
        slipped.name(),
        match named.is_empty() {
            true => "in one script".to_string(),
            false => named.join(" and "),
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(text: &str) -> BTreeSet<Script> {
        scripts(text)
    }

    #[test]
    fn a_word_in_a_script_the_conversation_never_used_is_the_slip_being_looked_for() {
        let conversation = known("Почини сборку, пожалуйста");
        let slipped_into = slipped(&scripts("Готово, я 修复 сборку"), &conversation);

        assert_eq!(slipped_into, Some(Script::Cjk));
        assert!(say_again(Script::Cjk, &conversation).contains("Cyrillic"), "and names what to go back to");
    }

    /// The common case, and the one a cruder check gets wrong: technical prose
    /// is Latin inside every other script, and always has been.
    #[test]
    fn latin_in_prose_of_another_script_is_not_a_slip() {
        let conversation = known("Посмотри в agent.rs");
        assert_eq!(slipped(&scripts("Правлю AgentLoop::run в agent.rs"), &conversation), None);
    }

    #[test]
    fn a_script_the_conversation_is_already_in_is_not_a_slip() {
        let conversation = known("这个 function 有 bug 吗？");
        assert_eq!(slipped(&scripts("有的，我修好了"), &conversation), None);
    }

    /// A file the agent read is quoted back inside backticks, and what is in
    /// somebody else's file is not the model changing language.
    #[test]
    fn what_is_quoted_as_code_is_not_the_models_own_voice() {
        let conversation = known("Что в этом файле?");
        assert_eq!(slipped(&scripts("Там строка `let greeting = \"你好\";`"), &conversation), None);
        assert_eq!(
            slipped(&scripts("Там вот что:\n```rust\nlet greeting = \"你好\";\n```\nи всё"), &conversation),
            None
        );
        assert_eq!(
            slipped(&scripts("Открыл файл, а там 你好"), &conversation),
            Some(Script::Cjk),
            "outside the backticks it is the model talking"
        );
    }

    #[test]
    fn an_unclosed_fence_swallows_the_rest_rather_than_reporting_it() {
        assert_eq!(scripts("текст\n```\n你好"), BTreeSet::from([Script::Cyrillic]));
    }
}
