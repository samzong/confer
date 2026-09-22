use super::*;

pub(super) async fn run_seat_worker(
    mut receiver: mpsc::UnboundedReceiver<QueuedDelivery>,
    store: StateStore,
    deliveries: DeliveryTracker,
    activity: Option<Arc<crate::status::Instance>>,
) {
    while let Some(queued) = receiver.recv().await {
        let session_guard = loop {
            match store.try_acquire_seat_lease(&queued.room_id, &queued.seat_id) {
                Ok(guard) => break Some(guard),
                Err(error) if error.to_string().starts_with("seat_busy:") => {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(error) => {
                    deliveries.set_failed(&queued.delivery_id, error.to_string());
                    break None;
                }
            }
        };
        let Some(_session_guard) = session_guard else {
            continue;
        };
        if let Err(error) =
            process_queued_delivery(&queued, &store, &deliveries, activity.as_ref()).await
        {
            deliveries.set_failed(&queued.delivery_id, error.to_string());
        }
    }
}

async fn process_queued_delivery(
    queued: &QueuedDelivery,
    store: &StateStore,
    deliveries: &DeliveryTracker,
    activity: Option<&Arc<crate::status::Instance>>,
) -> Result<()> {
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
    deliveries.set_running(&queued.delivery_id);
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
    };
    let output = adapters::run(invocation).await;
    let expected_session = reserved;
    let (mismatch, observed_session) = native_session_outcome(
        expected_session.as_deref(),
        output.observed_session_id.as_deref(),
    );
    let persistence_error =
        persist_native_session(store, &queued.room_id, &queued.seat_id, observed_session)
            .err()
            .map(|error| format!("native session could not be persisted: {error}"));
    deliveries.finish(
        &queued.delivery_id,
        mismatch,
        persistence_error,
        output.error,
        output.answer,
    );
    Ok(())
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
