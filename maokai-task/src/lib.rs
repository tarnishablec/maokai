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

pub enum TaskOp<E, T>
where
    T: Task<Event = E> + ?Sized + 'static,
{
    Start { handle: TaskHandle, task: Box<T> },
    Stop { handle: TaskHandle },
}

pub struct Reconciler<E, T>
where
    T: Task<Event = E> + ?Sized + 'static,
{
    next_handle: u64,
    pending: Vec<TaskOp<E, T>>,
}

impl<E, T> Default for Reconciler<E, T>
where
    T: Task<Event = E> + ?Sized + 'static,
{
    fn default() -> Self {
        Self {
            next_handle: 0,
            pending: Vec::new(),
        }
    }
}

impl<E, T> Reconciler<E, T>
where
    T: Task<Event = E> + ?Sized + 'static,
{
    pub fn start(&mut self, task: Box<T>) -> TaskHandle {
        let handle = TaskHandle(self.next_handle);
        self.next_handle += 1;
        self.pending.push(TaskOp::Start { handle, task });
        handle
    }

    pub fn stop(&mut self, handle: TaskHandle) {
        self.pending.push(TaskOp::Stop { handle });
    }

    pub fn drain(&mut self) -> Vec<TaskOp<E, T>> {
        core::mem::take(&mut self.pending)
    }
}

pub struct TaskContext<C, E, T>
where
    T: Task<Event = E> + ?Sized + 'static,
{
    context: C,
    reconciler: Reconciler<E, T>,
}

impl<C, E, T> TaskContext<C, E, T>
where
    T: Task<Event = E> + ?Sized + 'static,
{
    pub fn new(context: C) -> Self {
        Self {
            context,
            reconciler: Default::default(),
        }
    }

    pub fn reconciler(&mut self) -> &mut Reconciler<E, T> {
        &mut self.reconciler
    }
}

impl<C, E, T> Deref for TaskContext<C, E, T>
where
    T: Task<Event = E> + ?Sized + 'static,
{
    type Target = C;

    fn deref(&self) -> &Self::Target {
        &self.context
    }
}

impl<C, E, T> DerefMut for TaskContext<C, E, T>
where
    T: Task<Event = E> + ?Sized + 'static,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.context
    }
}
