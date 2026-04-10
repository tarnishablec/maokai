use alloc::boxed::Box;
use maokai_task::{Task, TaskHandle};

use crate::MailboxRuntime;

pub struct TokioMtRuntime<E: Send + 'static> {
    tx: tokio::sync::mpsc::UnboundedSender<(TaskHandle, E)>,
    rx: tokio::sync::mpsc::UnboundedReceiver<(TaskHandle, E)>,
}

impl<E: Send + 'static> TokioMtRuntime<E> {
    pub fn new() -> Self {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Self { tx, rx }
    }
}

impl<E: Send + 'static> Default for TokioMtRuntime<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: Send + 'static> MailboxRuntime<E> for TokioMtRuntime<E> {
    type Running = tokio::task::JoinHandle<()>;
    type Task = dyn Task<Event = E> + Send + 'static;

    fn start(&mut self, handle: TaskHandle, task: Box<Self::Task>) -> Self::Running {
        let tx = self.tx.clone();

        tokio::spawn(async move {
            let event = task.run().await;
            let _ = tx.send((handle, event));
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
    use std::time::Duration;

    use maokai_runner::Behaviors;
    use maokai_task::{Task, TaskContext};

    use super::TokioMtRuntime;
    use crate::{
        Mailbox, MailboxRuntime,
        runtimes::test_support::{Ctx, Event, IdleBehavior, LoadingBehavior, STATE_TREE},
    };

    type MtTask = <TokioMtRuntime<Event> as MailboxRuntime<Event>>::Task;

    struct MtSignalTask {
        gate: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }

    impl MtSignalTask {
        fn new(gate: tokio::sync::oneshot::Receiver<()>) -> Self {
            Self {
                gate: Mutex::new(Some(gate)),
            }
        }
    }

    #[async_trait::async_trait]
    impl Task for MtSignalTask {
        type Event = Event;

        async fn run(&self) -> Self::Event {
            let gate = self.gate.lock().unwrap().take().unwrap();
            let _ = gate.await;
            Event::Done
        }
    }

    fn build_behaviors(
        gate_slot: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
    ) -> Behaviors<'static, Event, TaskContext<Ctx, Event, MtTask>> {
        let mut behaviors = Behaviors::default();
        let (_, idle, loading) = &*STATE_TREE;

        behaviors.register(idle, IdleBehavior {});
        behaviors.register(
            loading,
            LoadingBehavior {
                make_task: {
                    let gate_slot = Arc::clone(&gate_slot);
                    move || MtSignalTask::new(gate_slot.lock().unwrap().take().unwrap())
                },
                wrap_task: |task| -> Box<MtTask> { Box::new(task) },
                marker: PhantomData,
            },
        );

        behaviors
    }

    #[test]
    fn state_tree_completion_flow_returns_to_idle() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .build()
            .unwrap();

        runtime.block_on(async {
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let gate_slot = Arc::new(Mutex::new(Some(done_rx)));
            let behaviors = build_behaviors(Arc::clone(&gate_slot));
            let (tree, idle, loading) = &*STATE_TREE;

            let mut mailbox = Mailbox::new(
                tree,
                &behaviors,
                *idle,
                Ctx::default(),
                TokioMtRuntime::<Event>::new(),
            );

            mailbox.post(Event::Begin);
            mailbox.run_until_stable();

            assert_eq!(mailbox.current(), *loading);
            assert!(mailbox.context().active_task.is_some());

            done_tx.send(()).unwrap();

            for _ in 0..64 {
                tokio::task::yield_now().await;
                tokio::time::sleep(Duration::from_millis(1)).await;
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
    fn state_tree_cancel_flow_aborts_mt_task() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .build()
            .unwrap();

        runtime.block_on(async {
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let gate_slot = Arc::new(Mutex::new(Some(done_rx)));
            let behaviors = build_behaviors(Arc::clone(&gate_slot));
            let (tree, idle, loading) = &*STATE_TREE;

            let mut mailbox = Mailbox::new(
                tree,
                &behaviors,
                *idle,
                Ctx::default(),
                TokioMtRuntime::<Event>::new(),
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
                tokio::time::sleep(Duration::from_millis(1)).await;
                mailbox.run_until_stable();
            }

            assert_eq!(mailbox.current(), *idle);
            assert!(mailbox.context().active_task.is_none());
        });
    }
}
