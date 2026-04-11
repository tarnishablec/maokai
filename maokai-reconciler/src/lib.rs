#![no_std]
extern crate alloc;

#[macro_use]
extern crate downcast;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::cmp::Ordering;
use downcast::Any;

pub trait Operation: Any {}

impl_downcast!(dyn Operation);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ticket(pub(crate) u32, pub(crate) u32);

impl Ticket {
    pub fn priority(self) -> u32 {
        self.1
    }

    pub fn index(self) -> u32 {
        self.0
    }
}

impl PartialOrd for Ticket {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Ticket {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .priority()
            .cmp(&self.priority())
            .then_with(|| self.index().cmp(&other.index()))
    }
}

pub enum PipelineFlow {
    Continue,
    Break,
}

pub enum IncomingDisposition {
    Keep,
    Drop,
}

pub type RuleResult = (PipelineFlow, IncomingDisposition);

pub trait Rule {
    fn apply(
        &self,
        incoming_ticket: Ticket,
        incoming: &mut dyn Operation,
        ctx: &mut dyn RuleAccess,
    ) -> RuleResult;
}

pub trait RuleAccess {
    fn unstage(&mut self, ticket: Ticket) -> Option<Box<dyn Operation>>;
    fn iter(&self) -> Box<dyn Iterator<Item = (Ticket, &dyn Operation)> + '_>;
    fn get_mut(&mut self, ticket: Ticket) -> Option<&mut dyn Operation>;
    fn replace(&mut self, ticket: Ticket, op: Box<dyn Operation>) -> Option<Box<dyn Operation>>;
}

#[derive(Default)]
pub struct Reconciler {
    next_ticket: u32,
    pub(crate) rules: Vec<Box<dyn Rule>>,
    pub(crate) pending_ops: BTreeMap<Ticket, Box<dyn Operation>>,
}

impl RuleAccess for Reconciler {
    fn unstage(&mut self, ticket: Ticket) -> Option<Box<dyn Operation>> {
        self.unstage(ticket)
    }

    fn iter(&self) -> Box<dyn Iterator<Item = (Ticket, &dyn Operation)> + '_> {
        Box::new(
            self.pending_ops
                .iter()
                .map(|(ticket, op)| (*ticket, op.as_ref())),
        )
    }

    fn get_mut(&mut self, ticket: Ticket) -> Option<&mut dyn Operation> {
        self.pending_ops.get_mut(&ticket).map(Box::as_mut)
    }

    fn replace(&mut self, ticket: Ticket, op: Box<dyn Operation>) -> Option<Box<dyn Operation>> {
        self.pending_ops.insert(ticket, op)
    }
}

impl Reconciler {
    pub(crate) fn next_ticket(&mut self, priority: u32) -> Ticket {
        let ticket = Ticket(self.next_ticket, priority);
        self.next_ticket += 1;
        ticket
    }

    pub fn stage<O: Operation + 'static>(
        &mut self,
        op: O,
        priority: Option<u32>,
    ) -> Option<Ticket> {
        let priority = priority.unwrap_or(0);
        let ticket = self.next_ticket(priority);
        let mut incoming = Box::new(op);
        let mut keep = true;

        let rules = core::mem::take(&mut self.rules);
        for rule in &rules {
            let (flow, disposition) = rule.apply(ticket, incoming.as_mut(), self);

            match disposition {
                IncomingDisposition::Keep => {}
                IncomingDisposition::Drop => {
                    keep = false;
                }
            }

            match flow {
                PipelineFlow::Break => break,
                PipelineFlow::Continue => {}
            }

            if !keep {
                break;
            }
        }
        self.rules = rules;

        if keep {
            self.pending_ops.insert(ticket, incoming);
            Some(ticket)
        } else {
            None
        }
    }

    pub fn unstage(&mut self, ticket: Ticket) -> Option<Box<dyn Operation>> {
        self.pending_ops.remove(&ticket)
    }

    pub fn clear(&mut self) {
        self.pending_ops.clear();
    }

    pub fn commit(&mut self, mut apply: impl FnMut(Ticket, Box<dyn Operation>)) {
        let pending = core::mem::take(&mut self.pending_ops);

        for (ticket, op) in pending {
            apply(ticket, op);
        }
    }

    pub fn add_rule<R>(&mut self, rule: R) -> &mut Self
    where
        R: Rule + 'static,
    {
        self.rules.push(Box::new(rule));
        self
    }
}
