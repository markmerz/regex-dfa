// Copyright 2015 Joe Neeman.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use engine::Engine;
use prefix::Prefix;
use program::{Program, InitStates};
use searcher::{Skipper, SkipToAsciiSet, SkipToByte, SkipToStr, AcSkipper, LoopSkipper, NoSkipper};
use std::mem;
use std::cell::RefCell;
use std::ops::DerefMut;

trait Initter {
    fn init_state(&self, last_char: Option<char>) -> Option<usize>;
}

impl<'a> Initter for &'a InitStates {
    fn init_state(&self, last_char: Option<char>) -> Option<usize> {
        self.state_after(last_char)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Thread {
    state: usize,
    start_idx: usize,
}

#[derive(Clone, Debug, PartialEq)]
struct Threads {
    threads: Vec<Thread>,
    states: Vec<u8>,
}

impl Threads {
    fn with_capacity(n: usize) -> Threads {
        Threads {
            threads: Vec::with_capacity(n),
            states: vec![0; n],
        }
    }

    fn add(&mut self, state: usize, start_idx: usize) {
        if self.states[state] == 0 {
            self.states[state] = 1;
            self.threads.push(Thread { state: state, start_idx: start_idx });
        }
    }

    fn starts_after(&self, start_idx: usize) -> bool {
        self.threads.is_empty() || self.threads[0].start_idx >= start_idx
    }
}

#[derive(Clone, Debug, PartialEq)]
struct ProgThreads {
    cur: Threads,
    next: Threads,
}

impl ProgThreads {
    fn with_capacity(n: usize) -> ProgThreads {
        ProgThreads {
            cur: Threads::with_capacity(n),
            next: Threads::with_capacity(n),
        }
    }

    fn swap(&mut self) {
        mem::swap(&mut self.cur, &mut self.next);
        self.next.threads.clear();
    }

    fn clear(&mut self) {
        self.cur.threads.clear();
        self.next.threads.clear();

        for s in &mut self.cur.states {
            *s = 0;
        }
        for s in &mut self.next.states {
            *s = 0;
        }
    }
}

#[derive(Clone, Debug)]
pub struct ThreadedEngine {
    prog: Program,
    threads: RefCell<ProgThreads>,
    prefix: Prefix,
}

impl ThreadedEngine {
    pub fn new(prog: Program) -> ThreadedEngine {
        let len = prog.insts.len();
        let pref = Prefix::extract(&prog);
        ThreadedEngine {
            prog: prog,
            threads: RefCell::new(ProgThreads::with_capacity(len)),
            prefix: pref,
        }
    }

    fn advance_thread(&self,
            threads: &mut ProgThreads,
            acc: &mut Option<(usize, usize)>,
            i: usize,
            ch: char,
            end: usize) {
        let state = threads.cur.threads[i].state;
        let start_idx = threads.cur.threads[i].start_idx;
        threads.cur.states[state] = 0;

        let (mut next_state, accept, retry) = self.prog.step(state, ch);
        if accept && (acc.is_none() || start_idx < acc.unwrap().0) {
            *acc = Some((start_idx, end));
        }
        // We're assuming here that we won't be asked to retry twice in a row, and if we
        // are asked to retry then there is no possibility of accepting afterwards.
        if retry {
            next_state = self.prog.step(next_state.unwrap(), ch).0;
        }
        if let Some(next_state) = next_state {
            threads.next.add(next_state, start_idx);
        }
    }

    fn shortest_match_<'a, Skip, Init>(&'a self, s: &str, skip: Skip, init: Init)
    -> Option<(usize, usize)>
    where Skip: Skipper + 'a, Init: Initter + 'a,
    {
        let mut acc: Option<(usize, usize)> = None;
        let (first_start_pos, mut pos, start_state) = match skip.skip(s, 0, None) {
            Some(x) => x,
            None => return None,
        };
        let mut threads_guard = self.threads.borrow_mut();
        let threads = threads_guard.deref_mut();

        threads.clear();
        threads.cur.threads.push(Thread { state: start_state, start_idx: first_start_pos });
        while pos < s.len() {
            let ch = s.char_at(pos);

            for i in 0..threads.cur.threads.len() {
                self.advance_thread(threads, &mut acc, i, ch, pos);
            }
            threads.swap();

            // If one of our threads accepted and it started sooner than any of our active
            // threads, we can stop early.
            if acc.is_some() && threads.cur.starts_after(acc.unwrap().0) {
                return acc;
            }

            // If we're out of threads, skip ahead to the next good position (but be sure to
            // always advance the input by at least one char).
            pos += ch.len_utf8();
            if threads.cur.threads.is_empty() {
                if let Some((next_start_pos, next_pos, state)) = skip.skip(s, pos, Some(ch)) {
                    pos = next_pos;
                    threads.cur.add(state, next_start_pos);
                } else {
                    return None
                }
            } else if let Some(state) = init.init_state(Some(ch)) {
                threads.cur.add(state, pos);
            }
        }

        for th in &threads.cur.threads {
            if self.prog.check_eoi(th.state) {
                return Some((th.start_idx, s.len()));
            }
        }
        None
    }

}

impl Engine for ThreadedEngine {
    fn shortest_match(&self, s: &str) -> Option<(usize, usize)> {
        if self.prog.insts.is_empty() {
            return None;
        }

        // TODO: see if we get better performance by specializing Initter
        match self.prefix {
            Prefix::AsciiChar(ref cs, state) =>
                self.shortest_match_(s, SkipToAsciiSet(cs.clone(), state), &self.prog.init),
            Prefix::Byte(b, state) =>
                self.shortest_match_(s, SkipToByte(b, state), &self.prog.init),
            Prefix::Lit(ref lit, state) =>
                self.shortest_match_(s, SkipToStr(lit, state), &self.prog.init),
            Prefix::Ac(ref ac, _) =>
                self.shortest_match_(
                    s,
                    AcSkipper(ac, self.prog.init.constant().unwrap()),
                    &self.prog.init),
            Prefix::LoopUntil(ref cs, state) =>
                self.shortest_match_(s, LoopSkipper(cs.clone(), state), &self.prog.init),
            Prefix::Empty => self.shortest_match_(s, NoSkipper(&self.prog.init), &self.prog.init),
        }
    }
}

