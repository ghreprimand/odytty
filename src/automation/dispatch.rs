// SPDX-License-Identifier: GPL-3.0-only
//! Bounded handoff to the existing event-loop owner. This owns requests, never
//! sessions. Cancellation and execution compete under the same state lock.

use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use super::protocol::{ErrorCode, Reply, Request, Response};

pub const MAX_QUEUED: usize = 32;
pub const MAX_PER_DISPATCH: usize = 8;
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const REQUESTS_PER_SECOND: usize = 32;

enum State {
    Pending(Request),
    Running,
    Completed(Response),
    Cancelled(ErrorCode),
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    deadline: Instant,
    request_id: u64,
    policy_epoch: u64,
}

/// A caller must retain this receipt until it receives a reply or explicitly
/// abandons the request. Dropping it cancels a request that has not begun.
pub struct Receipt(Arc<Shared>);

/// A connection monitor can cancel pending work while its worker waits for a
/// reply. Cancellation never pretends that an already-running mutation stopped.
#[derive(Clone)]
pub struct Cancellation(Arc<Shared>);

impl Cancellation {
    pub fn cancel(&self) -> bool {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*state, State::Pending(_)) {
            *state = State::Cancelled(ErrorCode::Cancelled);
            self.0.changed.notify_all();
            true
        } else {
            false
        }
    }
}

impl Receipt {
    /// Nonblocking observation for transports that monitor disconnects while
    /// awaiting the owner. Expiry uses the same lock as dispatch and cancel.
    pub fn try_response(&self) -> Option<Response> {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        match &*state {
            State::Completed(response) => return Some(response.clone()),
            State::Cancelled(error) => return Some(self.error(*error)),
            State::Pending(_) | State::Running => {}
        }
        if Instant::now() < self.0.deadline {
            return None;
        }
        let error = if matches!(*state, State::Pending(_)) {
            *state = State::Cancelled(ErrorCode::TimedOut);
            ErrorCode::TimedOut
        } else {
            ErrorCode::OutcomeUnknown
        };
        Some(self.error(error))
    }

    pub fn cancellation(&self) -> Cancellation {
        Cancellation(self.0.clone())
    }

    pub fn wait(self) -> Response {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            match &*state {
                State::Completed(response) => return response.clone(),
                State::Cancelled(error) => return self.error(*error),
                State::Pending(_) | State::Running => {}
            }
            let remaining = self.0.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                let error = if matches!(*state, State::Pending(_)) {
                    *state = State::Cancelled(ErrorCode::TimedOut);
                    ErrorCode::TimedOut
                } else {
                    ErrorCode::OutcomeUnknown
                };
                return self.error(error);
            }
            state = self
                .0
                .changed
                .wait_timeout(state, remaining)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    fn error(&self, code: ErrorCode) -> Response {
        Response {
            request_id: self.0.request_id,
            reply: Reply::Error(code),
        }
    }
}

impl Drop for Receipt {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(*state, State::Pending(_)) {
            *state = State::Cancelled(ErrorCode::Cancelled);
            self.0.changed.notify_all();
        }
    }
}

struct RateWindow {
    started: Instant,
    used: usize,
}

struct Policy {
    structural_control: bool,
    epoch: u64,
    stopped: bool,
}

#[derive(Clone)]
pub struct Submission {
    sender: mpsc::SyncSender<Arc<Shared>>,
    rate: Arc<Mutex<RateWindow>>,
    policy: Arc<Mutex<Policy>>,
}

/// The receiver remains with the GUI owner. Wake it through the existing
/// event-loop proxy after submitting; a failed wake drops/cancels the receipt.
pub struct DispatchQueue {
    receiver: mpsc::Receiver<Arc<Shared>>,
    policy: Arc<Mutex<Policy>>,
}

pub fn channel(structural_control: bool) -> (Submission, DispatchQueue) {
    let (sender, receiver) = mpsc::sync_channel(MAX_QUEUED);
    let policy = Arc::new(Mutex::new(Policy {
        structural_control,
        epoch: 0,
        stopped: false,
    }));
    (
        Submission {
            sender,
            rate: Arc::new(Mutex::new(RateWindow {
                started: Instant::now(),
                used: 0,
            })),
            policy: policy.clone(),
        },
        DispatchQueue { receiver, policy },
    )
}

impl Submission {
    pub fn submit(&self, request: Request) -> Result<Receipt, ErrorCode> {
        let policy = self.policy.lock().unwrap_or_else(|e| e.into_inner());
        if policy.stopped {
            return Err(ErrorCode::Unavailable);
        }
        if !policy.structural_control && !request.action.is_read_only() {
            return Err(ErrorCode::PermissionDenied);
        }
        // The codec validates even typed callers, so bypassing a transport
        // cannot admit oversized names or unsupported versions.
        super::protocol::encode(&request)?;
        let now = Instant::now();
        let mut rate = self.rate.lock().unwrap_or_else(|e| e.into_inner());
        if now.duration_since(rate.started) >= Duration::from_secs(1) {
            rate.started = now;
            rate.used = 0;
        }
        if rate.used >= REQUESTS_PER_SECOND {
            return Err(ErrorCode::Busy);
        }
        rate.used += 1;
        drop(rate);
        let shared = Arc::new(Shared {
            request_id: request.request_id,
            state: Mutex::new(State::Pending(request)),
            changed: Condvar::new(),
            deadline: now + REQUEST_TIMEOUT,
            policy_epoch: policy.epoch,
        });
        self.sender
            .try_send(shared.clone())
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => ErrorCode::Busy,
                mpsc::TrySendError::Disconnected(_) => ErrorCode::Unavailable,
            })?;
        Ok(Receipt(shared))
    }
}

impl DispatchQueue {
    /// Called by the same GUI owner that dispatches requests. An epoch change
    /// prevents disable/re-enable from reviving previously queued mutations.
    pub fn set_structural_control(&mut self, enabled: bool) {
        let mut policy = self.policy.lock().unwrap_or_else(|e| e.into_inner());
        if policy.stopped || policy.structural_control == enabled {
            return;
        }
        let Some(epoch) = policy.epoch.checked_add(1) else {
            policy.stopped = true;
            return;
        };
        policy.epoch = epoch;
        policy.structural_control = enabled;
    }

    /// Stop accepting requests and wake every queued caller deterministically.
    /// Endpoint teardown must also close its listener and cancel client tasks.
    pub fn shutdown(&mut self) {
        self.policy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopped = true;
        while let Ok(shared) = self.receiver.try_recv() {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if matches!(*state, State::Pending(_)) {
                *state = State::Cancelled(ErrorCode::Unavailable);
            }
            shared.changed.notify_all();
        }
    }

    /// Execute a bounded number of requests on the live window/session owner.
    /// The caller schedules another turn if this returns MAX_PER_DISPATCH.
    /// Expired or abandoned requests never reach the action callback.
    pub fn dispatch(&self, mut apply: impl FnMut(Request) -> Reply) -> usize {
        let mut handled = 0;
        while handled < MAX_PER_DISPATCH {
            let Ok(shared) = self.receiver.try_recv() else {
                break;
            };
            handled += 1;
            let request = {
                let policy = self.policy.lock().unwrap_or_else(|e| e.into_inner());
                let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                if policy.stopped {
                    *state = State::Cancelled(ErrorCode::Unavailable);
                    shared.changed.notify_all();
                    continue;
                }
                if let State::Pending(request) = &*state
                    && !request.action.is_read_only()
                    && (!policy.structural_control || policy.epoch != shared.policy_epoch)
                {
                    *state = State::Cancelled(ErrorCode::PermissionDenied);
                    shared.changed.notify_all();
                    continue;
                }
                if Instant::now() >= shared.deadline {
                    if matches!(*state, State::Pending(_)) {
                        *state = State::Cancelled(ErrorCode::TimedOut);
                    }
                    shared.changed.notify_all();
                    continue;
                }
                match std::mem::replace(&mut *state, State::Running) {
                    State::Pending(request) => request,
                    other => {
                        *state = other;
                        continue;
                    }
                }
            };
            let response = Response {
                request_id: shared.request_id,
                reply: apply(request),
            };
            *shared.state.lock().unwrap_or_else(|e| e.into_inner()) = State::Completed(response);
            shared.changed.notify_all();
        }
        handled
    }
}

impl Drop for DispatchQueue {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::protocol::{Action, VERSION};

    fn request(id: u64) -> Request {
        Request {
            request_id: id,
            version: VERSION,
            action: Action::List,
        }
    }

    #[test]
    fn abandoned_requests_do_not_run_and_live_requests_return_matching_ids() {
        let (submit, queue) = channel(false);
        drop(submit.submit(request(1)).unwrap());
        let live = submit.submit(request(2)).unwrap();
        let mut applied = Vec::new();
        assert_eq!(
            queue.dispatch(|request| {
                applied.push(request.request_id);
                Reply::Objects(vec![])
            }),
            2
        );
        assert_eq!(applied, [2]);
        assert_eq!(live.wait().request_id, 2);
    }

    #[test]
    fn queue_and_global_rate_are_bounded_across_clients() {
        let (submit, queue) = channel(false);
        let mut receipts = Vec::new();
        for id in 0..MAX_QUEUED {
            receipts.push(submit.clone().submit(request(id as u64)).unwrap());
        }
        assert!(matches!(submit.submit(request(40)), Err(ErrorCode::Busy)));
        assert_eq!(queue.dispatch(|_| Reply::Objects(vec![])), MAX_PER_DISPATCH);
        assert!(
            matches!(submit.submit(request(41)), Err(ErrorCode::Busy)),
            "draining does not reset the rate window"
        );
    }

    #[test]
    fn expired_queued_request_cannot_execute_later() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let shared = Arc::new(Shared {
            state: Mutex::new(State::Pending(request(1))),
            changed: Condvar::new(),
            deadline: Instant::now(),
            request_id: 1,
            policy_epoch: 0,
        });
        sender.send(shared.clone()).unwrap();
        assert_eq!(
            Receipt(shared).wait().reply,
            Reply::Error(ErrorCode::TimedOut)
        );
        let (_, mut queue) = channel(false);
        queue.receiver = receiver;
        queue.dispatch(|_| panic!("expired mutation ran"));
    }

    #[test]
    fn already_running_timeout_is_explicitly_indeterminate() {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::Running),
            changed: Condvar::new(),
            deadline: Instant::now(),
            request_id: 3,
            policy_epoch: 0,
        });
        assert_eq!(
            Receipt(shared).wait().reply,
            Reply::Error(ErrorCode::OutcomeUnknown)
        );
    }

    #[test]
    fn read_access_cannot_queue_structural_control() {
        let (submit, _queue) = channel(false);
        let mut value = request(1);
        value.action = Action::Rename {
            target: crate::automation::protocol::ObjectId {
                instance: [1; 16],
                kind: crate::automation::protocol::ObjectKind::Tab,
                serial: 1,
            },
            name: "New name".into(),
        };
        assert!(matches!(
            submit.submit(value),
            Err(ErrorCode::PermissionDenied)
        ));
    }

    #[test]
    fn disconnect_cancels_pending_work_while_receipt_waits() {
        let (submit, queue) = channel(false);
        let receipt = submit.submit(request(1)).unwrap();
        let cancellation = receipt.cancellation();
        let worker = std::thread::spawn(move || receipt.wait());
        assert!(cancellation.cancel());
        queue.dispatch(|_| panic!("disconnected request executed"));
        assert_eq!(
            worker.join().unwrap().reply,
            Reply::Error(ErrorCode::Cancelled)
        );
    }

    #[test]
    fn cancellation_cannot_claim_to_stop_an_already_running_request() {
        let (submit, queue) = channel(false);
        let receipt = submit.submit(request(1)).unwrap();
        let cancellation = receipt.cancellation();
        queue.dispatch(|_| {
            assert!(!cancellation.cancel());
            Reply::Objects(vec![])
        });
        assert_eq!(receipt.wait().reply, Reply::Objects(vec![]));
    }

    #[test]
    fn revoke_and_reenable_never_revives_a_queued_mutation() {
        let (submit, mut queue) = channel(true);
        let mut mutation = request(1);
        mutation.action = Action::Focus {
            target: crate::automation::protocol::ObjectId {
                instance: [1; 16],
                kind: crate::automation::protocol::ObjectKind::Window,
                serial: 1,
            },
        };
        let revoked = submit.submit(mutation).unwrap();
        let query = submit.submit(request(2)).unwrap();
        queue.set_structural_control(false);
        queue.set_structural_control(true);
        queue.dispatch(|request| {
            assert_eq!(request.request_id, 2, "revoked mutation ran");
            Reply::Objects(vec![])
        });
        assert_eq!(
            revoked.wait().reply,
            Reply::Error(ErrorCode::PermissionDenied)
        );
        assert_eq!(query.wait().reply, Reply::Objects(vec![]));
    }

    #[test]
    fn owner_teardown_wakes_waiters_and_rejects_new_requests() {
        let (submit, queue) = channel(false);
        let receipt = submit.submit(request(1)).unwrap();
        drop(queue);
        assert_eq!(receipt.wait().reply, Reply::Error(ErrorCode::Unavailable));
        assert!(matches!(
            submit.submit(request(2)),
            Err(ErrorCode::Unavailable)
        ));
    }
}
