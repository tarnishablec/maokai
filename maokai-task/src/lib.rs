#![no_std]
extern crate alloc;

use alloc::boxed::Box;
use alloc::vec::Vec;
use async_trait::async_trait;
use core::ops::{Deref, DerefMut};

#[async_trait]
pub trait Task: 'static {
    type Event;
    async fn run(&self) -> Self::Event;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskHandle(u64);

pub enum TaskOp<E> {
    Start {
        handle: TaskHandle,
        task: Box<dyn Task<Event = E>>,
    },
    Stop {
        handle: TaskHandle,
    },
}

pub struct Reconciler<E> {
    next_handle: u64,
    pending: Vec<TaskOp<E>>,
}

impl<E> Default for Reconciler<E> {
    fn default() -> Self {
        Self {
            next_handle: 0,
            pending: Vec::new(),
        }
    }
}

impl<E> Reconciler<E> {
    pub fn start(&mut self, task: Box<dyn Task<Event = E>>) -> TaskHandle {
        let handle = TaskHandle(self.next_handle);
        self.next_handle += 1;
        self.pending.push(TaskOp::Start { handle, task });
        handle
    }

    pub fn stop(&mut self, handle: TaskHandle) {
        self.pending.push(TaskOp::Stop { handle });
    }

    pub fn drain(&mut self) -> Vec<TaskOp<E>> {
        core::mem::take(&mut self.pending)
    }
}

pub struct WithTask<C, E> {
    context: C,
    reconciler: Reconciler<E>,
}

impl<C, E> WithTask<C, E> {
    pub fn new(context: C) -> Self {
        Self {
            context,
            reconciler: Default::default(),
        }
    }

    pub fn reconciler(&mut self) -> &mut Reconciler<E> {
        &mut self.reconciler
    }
}

impl<C, E> Deref for WithTask<C, E> {
    type Target = C;

    fn deref(&self) -> &Self::Target {
        &self.context
    }
}

impl<C, E> DerefMut for WithTask<C, E> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.context
    }
}
