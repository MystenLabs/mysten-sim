// Copyright (c) 2020-2021 Runji Wang
//

//! A simply priority-queue timer implementation.
//!
//! Code originally from https://github.com/rcore-os/naive-timer (MIT license)

/// A naive timer.
#[derive(Default)]
pub struct Timer {
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
        deadline: Duration,
        callback: impl FnOnce(Duration) + Send + Sync + 'static,
    ) {
        let event = Event {
            deadline,
            callback: Box::new(callback),
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
            (event.callback)(now);
        }
    }

    /// Get next timer.
    pub fn next(&self) -> Option<Duration> {
        self.events.peek().map(|e| e.deadline)
    }
}

struct Event {
    deadline: Duration,
    callback: Callback,
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
