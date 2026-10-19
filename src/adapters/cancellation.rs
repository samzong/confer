use tokio::sync::watch;
use tokio::time::Instant;

#[derive(Clone, Copy, Debug, Default)]
struct State {
    deadline: Option<Instant>,
    finished: bool,
    uncertain: bool,
    process_id: Option<u32>,
}

#[derive(Clone, Debug)]
pub(crate) struct Cancellation(watch::Sender<State>);

impl Default for Cancellation {
    fn default() -> Self {
        Self(watch::channel(State::default()).0)
    }
}

impl Cancellation {
    pub(crate) fn cancel(&self) {
        self.0.send_if_modified(|state| {
            if state.deadline.is_some() || state.finished {
                return false;
            }
            state.deadline = Some(Instant::now() + std::time::Duration::from_secs(3));
            true
        });
    }

    pub(super) fn set_process_id(&self, process_id: u32) {
        self.0
            .send_modify(|state| state.process_id = Some(process_id));
    }

    #[cfg(unix)]
    pub(super) fn process_id(&self) -> Option<u32> {
        self.0.borrow().process_id
    }

    pub(crate) fn finish(&self) {
        self.0.send_modify(|state| state.finished = true);
    }

    pub(crate) fn mark_uncertain(&self) {
        self.0.send_modify(|state| state.uncertain = true);
    }

    pub(crate) fn is_uncertain(&self) -> bool {
        self.0.borrow().uncertain
    }

    #[cfg(test)]
    pub(super) fn is_finished(&self) -> bool {
        self.0.borrow().finished
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.borrow().deadline.is_some()
    }

    pub(crate) async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        let _ = receiver.wait_for(|state| state.deadline.is_some()).await;
    }

    pub(super) async fn grace_expired(&self) {
        self.cancelled().await;
        let deadline = self.0.borrow().deadline.expect("cancellation deadline");
        tokio::time::sleep_until(deadline).await;
    }
}
