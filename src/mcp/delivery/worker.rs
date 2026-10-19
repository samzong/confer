use super::*;

pub(super) async fn run_seat_worker(
    mut receiver: mpsc::UnboundedReceiver<QueuedDelivery>,
    store: StateStore,
    deliveries: DeliveryTracker,
    activity: Option<Arc<crate::mcp::activity::Instance>>,
) {
    while let Some(queued) = receiver.recv().await {
        if queued.cancellation.is_cancelled() {
            continue;
        }
        let session_guard = loop {
            match store.try_acquire_seat_lease(&queued.room_id, &queued.seat_id) {
                Ok(guard) => break Some(guard),
                Err(error) if error.to_string().starts_with("seat_busy:") => {
                    tokio::select! {
                        _ = queued.cancellation.cancelled() => break None,
                        _ = tokio::time::sleep(Duration::from_millis(50)) => {}
                    }
                }
                Err(error) => {
                    deliveries.set_failed(&queued.delivery_id, error.to_string());
                    break None;
                }
            }
        };
        let Some(lease) = session_guard else {
            continue;
        };
        let result = match mark_seat_dispatch(&lease, &queued.delivery_id) {
            Ok(()) => {
                process_queued_delivery(&queued, &store, &deliveries, activity.as_ref()).await
            }
            Err(error) => Err(anyhow::anyhow!(
                "failed to record delivery dispatch: {error}"
            )),
        };
        let preserve_uncertainty = match &result {
            Ok(completion) => {
                completion.persistence_error.is_some()
                    || completion.mismatch.is_some()
                    || (queued.cancellation.is_cancelled() && !completion.session_known)
            }
            Err(_) => queued.cancellation.is_cancelled(),
        };
        let dispatched = deliveries
            .states
            .borrow()
            .get(&queued.delivery_id)
            .is_some_and(|delivery| delivery.status == DeliveryStatus::Running);
        let preserve_uncertainty = dispatched
            && queued.cancellation.is_cancelled()
            && (preserve_uncertainty || queued.cancellation.is_uncertain());
        if preserve_uncertainty {
            deliveries.update(&queued.delivery_id, |delivery| {
                delivery.previous_delivery_uncertain = true;
            });
        }
        if !preserve_uncertainty {
            let _ = lease.set_len(0);
        }
        drop(lease);
        match result {
            Ok(completion) => deliveries.finish(
                &queued.delivery_id,
                completion.mismatch,
                completion.persistence_error,
                completion.output.error,
                completion.output.answer,
                completion.resume_command,
            ),
            Err(error) => deliveries.set_failed(&queued.delivery_id, format!("{error:#}")),
        }
    }
}

async fn process_queued_delivery(
    queued: &QueuedDelivery,
    store: &StateStore,
    deliveries: &DeliveryTracker,
    activity: Option<&Arc<crate::mcp::activity::Instance>>,
) -> Result<Completion> {
    let room = store.room_for_workspace(&queued.room_id, &queued.workspace)?;
    let seat = room
        .seats
        .iter()
        .find(|seat| seat.id == queued.seat_id)
        .with_context(|| format!("seat '{}' disappeared before delivery", queued.seat_id))?;
    if seat.status == SeatStatus::Retired {
        bail!("seat '{}' is retired", seat.name);
    }
    let readiness = adapters::check_readiness(seat.agent);
    if !readiness.locally_ready {
        bail!(
            readiness
                .reason
                .unwrap_or_else(|| "agent is not locally ready".into())
        );
    }
    if !deliveries.set_running(&queued.delivery_id) {
        bail!("delivery was cancelled before dispatch");
    }
    let _activity = activity.map(|instance| instance.start(&queued.delivery_id, &room, seat));
    let first_message = seat.native_session_id.is_none();
    let executable = readiness
        .executable
        .map(PathBuf::from)
        .context("agent readiness returned no executable")?;
    let reserved = if first_message {
        adapters::reserve_session(seat.agent)
    } else {
        seat.native_session_id.clone()
    };
    let invocation = Invocation {
        agent: seat.agent,
        executable,
        workspace: queued.workspace.clone(),
        native_session_id: reserved.clone(),
        model: seat.model.clone(),
        reasoning_effort: seat.reasoning_effort.clone(),
        instructions: seat.instructions.clone(),
        message: queued.message.clone(),
        first_message,
        cancellation: queued.cancellation.clone(),
    };
    let mut output = adapters::run(invocation).await;
    if queued.cancellation.is_cancelled()
        && output.observed_session_id.is_none()
        && seat.native_session_id.is_none()
    {
        let detail =
            "no native session was established; the next delivery will create a new native session";
        output.error = Some(match output.error {
            Some(error) => format!("{error}; {detail}"),
            None => detail.into(),
        });
    }
    let expected_session = reserved;
    let (mismatch, observed_session) = native_session_outcome(
        expected_session.as_deref(),
        output.observed_session_id.as_deref(),
    );
    let persistence_error =
        persist_native_session(store, &queued.room_id, &queued.seat_id, observed_session)
            .err()
            .map(|error| format!("native session could not be persisted: {error:#}"));
    let resume_command = observed_session
        .filter(|_| persistence_error.is_none())
        .or(seat.native_session_id.as_deref())
        .and_then(|session| adapters::resume_command(seat.agent, &room.workspace, session));
    let session_known = persistence_error.is_none()
        && mismatch.is_none()
        && (observed_session.is_some() || seat.native_session_id.is_some());
    Ok(Completion {
        session_known,
        mismatch,
        persistence_error,
        output,
        resume_command,
    })
}

struct Completion {
    session_known: bool,
    mismatch: Option<String>,
    persistence_error: Option<String>,
    output: adapters::AdapterOutput,
    resume_command: Option<String>,
}

fn persist_native_session(
    store: &StateStore,
    room_id: &str,
    seat_id: &str,
    native_session_id: Option<&str>,
) -> Result<()> {
    let Some(native_session_id) = native_session_id else {
        return Ok(());
    };
    store.mutate(|state| {
        let room = state
            .rooms
            .iter_mut()
            .find(|room| room.id == room_id)
            .with_context(|| format!("room '{room_id}' disappeared while starting a session"))?;
        let seat = room
            .seats
            .iter_mut()
            .find(|seat| seat.id == seat_id)
            .with_context(|| format!("seat '{seat_id}' disappeared while starting a session"))?;
        match seat.native_session_id.as_deref() {
            Some(existing) if existing != native_session_id => {
                bail!(
                    "seat '{}' already targets native session '{existing}'",
                    seat.name
                )
            }
            _ => seat.native_session_id = Some(native_session_id.into()),
        }
        room.updated_at = timestamp();
        Ok(())
    })
}

pub(super) fn native_session_outcome<'a>(
    expected: Option<&str>,
    observed: Option<&'a str>,
) -> (Option<String>, Option<&'a str>) {
    let mismatch = expected.zip(observed).and_then(|(expected, observed)| {
        (expected != observed)
            .then(|| format!("native session changed from '{expected}' to '{observed}'"))
    });
    let persistable = mismatch.is_none().then_some(observed).flatten();
    (mismatch, persistable)
}
