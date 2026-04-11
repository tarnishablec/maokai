use maokai_reconciler::Operation;

pub enum EventOp<E> {
    Emit(E),
}

impl<E: 'static> Operation for EventOp<E> {}
