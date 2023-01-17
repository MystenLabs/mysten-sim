// Copyright (c) 2020-2021 Runji Wang
//
// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

//! A simply priority-queue timer implementation.
//!
//! Code originally from https://github.com/rcore-os/naive-timer (MIT license)

use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};
use std::time::Duration;

use crate::task::NodeId;

use tracing::debug;

/// A naive timer.
#[derive(Default)]
pub struct Timer {
    disabled_node_ids: HashSet<NodeId>,
    events: BinaryHeap<Event>,
}

/// The type of callback function.
type Callback = Box<dyn FnOnce(Duration) + Send + Sync + 'static>;

impl Timer {
    /// Add a timer.
    ///
    /// The `callback` will be called on timer expired after `deadline`.
    pub fn add(
        &mut self,
        node_id: NodeId,
        deadline: Duration,
        callback: impl FnOnce(Duration) + Send + Sync + 'static,
    ) {
        if self.disabled_node_ids.contains(&node_id) {
            debug!("not scheduling event for deleted node {node_id}");
            return;
        }

        let event = Event {
            deadline,
            node_id,
            callback: Cell::new(Some(Box::new(callback))),
        };
        self.events.push(event);
    }

    /// Expire timers.
    ///
    /// Given the current time `now`, trigger and remove all expired timers.
    pub fn expire(&mut self, now: Duration) {
        while let Some(t) = self.events.peek() {
            if t.deadline > now {
                break;
            }
            let event = self.events.pop().unwrap();

            // event may have been cancelled
            if let Some(callback) = event.callback.take() {
                (callback)(now);
            }
        }
    }

    /// Remove all events for node and return them in a vector. The vector should be
    /// dropped while no locks are held, since drop handlers may acquire arbitrary locks.
    pub fn disable_node_and_remove_events(&mut self, node_id: NodeId) -> Vec<Callback> {
        assert!(self.disabled_node_ids.insert(node_id));
        let mut ret = Vec::new();
        for entry in self.events.iter() {
            if entry.node_id == node_id {
                if let Some(cb) = entry.callback.take() {
                    ret.push(cb);
                }
            }
        }
        ret
    }

    pub fn enable_node(&mut self, node_id: NodeId) {
        assert!(self.disabled_node_ids.remove(&node_id));
    }

    /// Get next timer.
    pub fn next(&self) -> Option<Duration> {
        self.events.peek().map(|e| e.deadline)
    }
}

struct Event {
    deadline: Duration,
    node_id: NodeId,
    callback: Cell<Option<Callback>>,
}

impl PartialEq for Event {
    fn eq(&self, other: &Self) -> bool {
        self.deadline.eq(&other.deadline)
    }
}

impl Eq for Event {}

// BinaryHeap is a max-heap. So we need to reverse the order.
impl PartialOrd for Event {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        other.deadline.partial_cmp(&self.deadline)
    }
}

impl Ord for Event {
    fn cmp(&self, other: &Event) -> Ordering {
        other.deadline.cmp(&self.deadline)
    }
}
