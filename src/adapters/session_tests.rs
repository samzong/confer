use super::*;
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn exited_cli_observer_failure_does_not_leave_uncertainty() {
    for agent in [AgentKind::Claude, AgentKind::Agy] {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("agent");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s\\n' '{\"session_id\":\"native-session\",\"result\":\"answer\"}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cancellation = Cancellation::default();
        let output = run(Invocation {
            agent,
            executable,
            workspace: directory.path().to_owned(),
            native_session_id: None,
            model: None,
            reasoning_effort: None,
            instructions: None,
            message: "work".into(),
            first_message: true,
            cancellation: cancellation.clone(),
            session_observer: SessionObserver::new(|_| {
                anyhow::bail!("fixture observation failure")
            }),
        })
        .await;
        assert!(
            output
                .error
                .as_deref()
                .unwrap()
                .contains("fixture observation failure"),
            "{output:?}"
        );
        assert!(!cancellation.is_cancelled());
        assert!(!cancellation.is_uncertain(), "{agent:?}: {output:?}");
    }
}

#[tokio::test]
async fn cli_reports_identity_before_the_native_process_finishes() {
    for agent in [AgentKind::Claude, AgentKind::Agy] {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("agent");
        std::fs::write(
            &executable,
            r#"#!/bin/sh
printf '%s\n' '{"session_id":"streamed-native-session","type":"system"}'
while ! test -f release; do sleep 0.01; done
printf '%s\n' '{"session_id":"streamed-native-session","type":"result","result":"finished"}'
"#,
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let invocation = Invocation {
            agent,
            executable,
            workspace: directory.path().to_owned(),
            native_session_id: None,
            model: None,
            reasoning_effort: None,
            instructions: None,
            message: "work".into(),
            first_message: true,
            cancellation: Default::default(),
            session_observer: SessionObserver::new(move |id| {
                sender.send(id.to_owned())?;
                Ok(())
            }),
        };
        let task = tokio::spawn(run(invocation));
        let observed = tokio::time::timeout(std::time::Duration::from_secs(3), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        let still_running = !task.is_finished();
        std::fs::write(directory.path().join("release"), "yes").unwrap();
        let output = task.await.unwrap();
        assert!(still_running);
        assert_eq!(observed, "streamed-native-session");
        assert_eq!(output.answer.as_deref(), Some("finished"));
        assert!(output.error.is_none(), "{output:?}");
        assert!(
            receiver.try_recv().is_err(),
            "the same session ID was reported twice"
        );
    }
}
