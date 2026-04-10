use alloc::boxed::Box;
use maokai_task::{Task, TaskHandle};

use crate::MailboxRuntime;

pub struct TokioLocalRuntime<E: 'static> {
    tx: tokio::sync::mpsc::UnboundedSender<(TaskHandle, E)>,
    rx: tokio::sync::mpsc::UnboundedReceiver<(TaskHandle, E)>,
}

impl<E: 'static> TokioLocalRuntime<E> {
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self { tx, rx }
    }
}

impl<E: 'static> Default for TokioLocalRuntime<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: 'static> MailboxRuntime<E> for TokioLocalRuntime<E> {
    type Running = tokio::task::JoinHandle<()>;
    type Task = dyn Task<Event = E> + 'static;

    fn start(&mut self, handle: TaskHandle, task: Box<dyn Task<Event = E>>) -> Self::Running {
        let tx = self.tx.clone();

        // `Task` is not required to be `Send`, so the local Tokio runtime uses `spawn_local`.
        tokio::task::spawn_local(async move {
            let any_event = task.run().await;

            let _ = tx.send((handle, any_event));
        })
    }

    fn stop(&mut self, running: Self::Running) {
        running.abort();
    }

    fn poll(&mut self) -> Option<(TaskHandle, E)> {
        self.rx.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    extern crate alloc;
    extern crate std;

    use alloc::boxed::Box;
    use core::marker::PhantomData;
    use std::sync::{Arc, Mutex};

    use maokai_runner::Behaviors;
    use maokai_task::{Task, TaskContext};

    use super::TokioLocalRuntime;
    use crate::{
        Mailbox, MailboxRuntime,
        runtimes::test_support::{Ctx, Event, IdleBehavior, LoadingBehavior, STATE_TREE},
    };

    type LocalTask = <TokioLocalRuntime<Event> as MailboxRuntime<Event>>::Task;

    struct LocalSignalTask {
        gate: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }

    impl LocalSignalTask {
        fn new(gate: tokio::sync::oneshot::Receiver<()>) -> Self {
            Self { gate: Mutex::new(Some(gate)) }
        }
    }

    #[async_trait::async_trait]
    impl Task for LocalSignalTask {
        type Event = Event;

        async fn run(&self) -> Self::Event {
            let gate = self.gate.lock().unwrap().take().unwrap();
            let _ = gate.await;
            Event::Done
        }
    }

    fn build_behaviors(
        gate_slot: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
    ) -> Behaviors<'static, Event, TaskContext<Ctx, Event, LocalTask>> {
        let mut behaviors: Behaviors<'static, Event, TaskContext<Ctx, Event, LocalTask>> =
            Behaviors::default();
        let (_, idle, loading) = &*STATE_TREE;

        behaviors.register(idle, IdleBehavior {});
        behaviors.register(
            loading,
            LoadingBehavior {
                make_task: {
                    let gate_slot = Arc::clone(&gate_slot);
                    move || LocalSignalTask::new(gate_slot.lock().unwrap().take().unwrap())
                },
                wrap_task: |task| -> Box<LocalTask> { Box::new(task) },
                marker: PhantomData,
            },
        );

        behaviors
    }

    #[test]
    fn state_tree_completion_flow_returns_to_idle() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();

        local.block_on(&runtime, async {
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let gate_slot = Arc::new(Mutex::new(Some(done_rx)));
            let behaviors = build_behaviors(Arc::clone(&gate_slot));
            let (tree, idle, loading) = &*STATE_TREE;

            let mut mailbox = Mailbox::new(
                tree,
                &behaviors,
                *idle,
                Ctx::default(),
                TokioLocalRuntime::<Event>::new(),
            );

            mailbox.post(Event::Begin);
            mailbox.run_until_stable();

            assert_eq!(mailbox.current(), *loading);
            assert!(mailbox.context().active_task.is_some());

            done_tx.send(()).unwrap();

            for _ in 0..16 {
                tokio::task::yield_now().await;
                mailbox.run_until_stable();
                if mailbox.current() == *idle {
                    break;
                }
            }

            assert_eq!(mailbox.current(), *idle);
            assert!(mailbox.context().active_task.is_none());
        });
    }

    #[test]
    fn state_tree_cancel_flow_aborts_local_task() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();

        local.block_on(&runtime, async {
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let gate_slot = Arc::new(Mutex::new(Some(done_rx)));
            let behaviors = build_behaviors(Arc::clone(&gate_slot));
            let (tree, idle, loading) = &*STATE_TREE;

            let mut mailbox = Mailbox::new(
                tree,
                &behaviors,
                *idle,
                Ctx::default(),
                TokioLocalRuntime::<Event>::new(),
            );

            mailbox.post(Event::Begin);
            mailbox.run_until_stable();

            assert_eq!(mailbox.current(), *loading);
            assert!(mailbox.context().active_task.is_some());

            mailbox.post(Event::Cancel);
            mailbox.run_until_stable();

            assert_eq!(mailbox.current(), *idle);
            assert!(mailbox.context().active_task.is_none());

            done_tx.send(()).unwrap();

            for _ in 0..16 {
                tokio::task::yield_now().await;
                mailbox.run_until_stable();
            }

            assert_eq!(mailbox.current(), *idle);
            assert!(mailbox.context().active_task.is_none());
        });
    }
}
