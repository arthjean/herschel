// SPDX-FileCopyrightText: 2026 Arthur Jean
// SPDX-License-Identifier: GPL-3.0-or-later

//! What the two write probes share: the person watching, and their consent.
//!
//! Both probes send something this product has never validated on the firmware
//! in front of it, and neither device answers a report that reads its state
//! back. So both depend on the same instrument, which is an operator with their
//! eyes on the hardware, and both are gated by the same act, which is that
//! operator typing a phrase after being told what is about to be sent.
//!
//! One operator rather than one per probe: they were the same trait with the
//! same terminal implementation under two names, which meant `korid` had two
//! types for one thing and a fix to the prompt could land on one of them.

use std::io::{BufRead, Write};

/// What the operator must type to authorize a write this product has not
/// validated on this firmware.
pub const CONFIRMATION_PHRASE: &str = "PROBE";

/// Where a probe asks its questions and reads the answers.
///
/// A trait so the ordering, the refusal and the recording can be tested without
/// a terminal, and so a run with no answers available records empty
/// observations rather than inventing them.
pub trait Operator {
    /// Ask a question and return the answer, trimmed.
    fn ask(&mut self, question: &str) -> String;

    /// Report progress. Never carries a value the record depends on.
    fn note(&mut self, message: &str);
}

/// The interactive operator, on stderr and stdin.
///
/// Prompts go to stderr so the report itself can be redirected to a file
/// without the questions landing in it.
pub struct Terminal;

impl Operator for Terminal {
    fn ask(&mut self, question: &str) -> String {
        eprint!("{question} ");
        let _ = std::io::stderr().flush();
        let mut answer = String::new();
        match std::io::stdin().lock().read_line(&mut answer) {
            Ok(0) | Err(_) => String::new(),
            Ok(_) => answer.trim().to_string(),
        }
    }

    fn note(&mut self, message: &str) {
        eprintln!("{message}");
    }
}

/// Ask for the typed confirmation, and say whether it was given.
///
/// Anything other than the exact phrase is a refusal, including an empty line
/// from an operator who walked away and a terminal with no one at it.
pub fn authorized<O: Operator + ?Sized>(operator: &mut O) -> bool {
    operator.ask(&format!(
        "Type {CONFIRMATION_PHRASE} to authorize, anything else to abort:"
    )) == CONFIRMATION_PHRASE
}

/// An operator with a scripted set of answers.
///
/// Lives beside the trait rather than in each probe's tests because the two
/// probes had the same one twice. Both of them assert on ordering, so what is
/// asked and what is said are both kept in the order they happened.
#[cfg(test)]
pub mod testing {
    use super::Operator;

    pub struct Script {
        answers: std::collections::VecDeque<String>,
        pub asked: Vec<String>,
        pub noted: Vec<String>,
    }

    impl Script {
        pub fn new(answers: &[&str]) -> Self {
            Self {
                answers: answers.iter().map(|answer| answer.to_string()).collect(),
                asked: Vec::new(),
                noted: Vec::new(),
            }
        }

        /// What the operator had been told, joined for a single assertion.
        pub fn briefing(&self) -> String {
            self.noted.join(" ")
        }
    }

    impl Operator for Script {
        fn ask(&mut self, question: &str) -> String {
            self.asked.push(question.to_string());
            self.answers.pop_front().unwrap_or_default()
        }

        fn note(&mut self, message: &str) {
            self.noted.push(message.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Script;
    use super::*;

    #[test]
    fn only_the_exact_phrase_authorizes_a_run() {
        for answer in ["", "yes", "probe", "PROBE please"] {
            let mut operator = Script::new(&[answer]);
            assert!(
                !authorized(&mut operator),
                "{answer:?} must not authorize a write"
            );
        }
        assert!(authorized(&mut Script::new(&[CONFIRMATION_PHRASE])));
    }

    #[test]
    fn an_operator_who_answers_nothing_at_all_refuses() {
        // An empty queue is a terminal nobody is at. It reads as a refusal,
        // never as consent.
        assert!(!authorized(&mut Script::new(&[])));
    }
}
