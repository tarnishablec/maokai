#![no_std]
extern crate alloc;

use maokai_reconciler::Reconciler;
use maokai_runner::Runner;
use maokai_tree::State;

pub struct Envelope<C> {
    context: C,
    reconciler: Reconciler,
}

pub struct Mailbox<'a, T> {
    runner: Runner<'a, T>,
    current: State,
}

impl<T> Mailbox<'_, T> {
    pub fn step() {}
}
