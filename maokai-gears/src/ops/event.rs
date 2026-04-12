use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use core::cell::RefCell;
use downcast::Downcast;
use maokai_reconciler::{OpConsumer, OpFlow, Operation, Ticket};

pub enum EventOp<E> {
    Emit(E),
}

impl<E: 'static> Operation for EventOp<E> {}

//

pub type SharedEventQueue<E> = Rc<RefCell<VecDeque<E>>>;

pub struct EventOpConsumer<E> {
    queue: SharedEventQueue<E>,
}

impl<E> EventOpConsumer<E> {
    pub fn new(queue: SharedEventQueue<E>) -> Self {
        Self { queue }
    }
}

impl<E: 'static> OpConsumer for EventOpConsumer<E> {
    fn consume(&mut self, _: Ticket, op: Box<dyn Operation>) -> OpFlow {
        match Downcast::<EventOp<E>>::downcast(op) {
            Ok(event_op) => {
                match *event_op {
                    EventOp::Emit(event) => self.queue.borrow_mut().push_back(event),
                }
                OpFlow::Consumed
            }
            Err(err) => OpFlow::Continue(err.into_object()),
        }
    }
}
