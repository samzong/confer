use super::*;
use crate::mcp::api::SendMessageArgs;
use crate::test_support::{room, seat};
use std::os::unix::fs::PermissionsExt;

#[tokio::test]
async fn caller_cancellation_keeps_cancelled_when_question_arrives() {
    let directory = tempfile::tempdir().unwrap();
    let workspace = directory.path().canonicalize().unwrap();
    let store = StateStore::new(directory.path().join("rooms.json"));
    store
        .mutate(|state| {
            state
                .rooms
                .push(room("room-1", &workspace.to_string_lossy()));
            Ok(())
        })
        .unwrap();
    let server = ConferMcp::with_store(store);
    let delivery = super::tests::delivery("cancel-race", DeliveryStatus::Running);
    let signal = delivery.cancellation.clone();
    server.runtime.deliveries.insert(delivery);
    let cancelling_server = server.clone();
    let cancellation = tokio::spawn(async move {
        cancelling_server
            .cancel_delivery_inner(crate::mcp::api::CancelDeliveryArgs {
                workspace,
                room_id: "room-1".into(),
                delivery_ids: vec!["cancel-race".into()],
            })
            .await
            .unwrap()
    });
    signal.cancelled().await;
    let question = serde_json::json!({"questions":[{"id":"q1","question":"Which target?"}]});
    signal.cancel();
    signal.finish();
    server.runtime.deliveries.finish(
        "cancel-race",
        None,
        None,
        adapters::AdapterOutput {
            observed_session_id: Some("native-session".into()),
            answer: None,
            error: None,
            question: Some(question.clone()),
        },
        None,
    );
    let result = cancellation.await.unwrap();
    assert!(result.completed);
    assert!(!result.timed_out);
    assert_eq!(result.deliveries[0].status, DeliveryStatus::Cancelled);
    assert_eq!(result.deliveries[0].question.as_ref(), Some(&question));
    assert!(result.deliveries[0].error.is_none());
}

#[tokio::test]
async fn questions_return_to_host_without_fabricated_answers() {
    if std::env::var_os("CONFER_TEST_QUESTIONS").is_none() {
        let directory = tempfile::tempdir().unwrap();
        let script = r#"#!/usr/bin/env python3
import json,sys,os
codex=os.path.basename(sys.argv[0])=='codex'
sid='codex-native' if codex else 'cursor-native'
def send(value):
    value['jsonrpc']='2.0'
    print(json.dumps(value),flush=True)
def response(value,result):
    send({'id':value['id'],'result':result})
question_request=900
pending=None
for line in sys.stdin:
    v=json.loads(line)
    m=v.get('method')
    with open('wire','a') as f:f.write(json.dumps(v)+'\n')
    if m=='initialize':
        response(v,{} if codex else {'protocolVersion':1,'agentCapabilities':{'loadSession':True}})
    elif m in ('thread/start','thread/resume'):
        if m=='thread/resume': assert v['params']['threadId']==sid
        response(v,{'thread':{'id':sid}})
    elif m in ('session/new','session/load'):
        if m=='session/load': assert v['params']['sessionId']==sid
        response(v,{'sessionId':sid})
    elif m in ('turn/start','session/prompt'):
        text=v['params']['input' if codex else 'prompt'][0]['text']
        stall='failed-cleanup' in text
        if 'host-answer' in text:
            if codex:
                response(v,{'turn':{'id':'turn-2'}})
                send({'method':'turn/completed','params':{'threadId':sid,'turn':{'id':'turn-2','status':'completed','items':[{'type':'agentMessage','text':'answer received'}]}}})
            else:
                send({'method':'session/update','params':{'sessionId':sid,'update':{'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'answer received'}}}})
                response(v,{'stopReason':'end_turn'})
        else:
            pending=v
            if codex:
                response(v,{'turn':{'id':'turn-1'}})
                params={'threadId':sid,'turnId':'turn-1','itemId':'question-1','isBlocking':True,'questions':[{'id':'q1','header':'Confirm','question':'Which target?','isOther':False,'isSecret':False,'options':[{'label':'A','description':'First target'}]}]}
            else:
                params={'sessionId':sid,'toolCallId':'question-1','title':'Confirm','questions':[{'id':'q1','prompt':'Which target?','options':[{'id':'a','label':'A'}],'allowMultiple':False}]}
            send({'id':question_request,'method':'item/tool/requestUserInput' if codex else '_cursor/ask_question','params':params})
    elif m=='turn/interrupt':
        if stall: continue
        response(v,{})
        send({'method':'turn/completed','params':{'threadId':sid,'turn':{'id':'turn-1','status':'interrupted','items':[]}}})
    elif m=='thread/unsubscribe': response(v,{})
    elif m=='session/cancel':
        assert pending is not None
    elif v.get('id')==question_request:
        if codex: raise Exception('Confer fabricated a Codex answer')
        assert v.get('result')=={'outcome':{'outcome':'cancelled'}},v
        if not stall: response(pending,{'stopReason':'cancelled'})
"#;
        for binary in ["codex", "agent"] {
            let path = directory.path().join(binary);
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = std::env::join_paths(
            std::iter::once(directory.path().to_owned())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "mcp::delivery::question_tests::questions_return_to_host_without_fabricated_answers", "--nocapture"])
            .env("CONFER_TEST_QUESTIONS", directory.path()).env("PATH", path)
            .env("OPENAI_API_KEY", "fixture-not-a-credential").env("CURSOR_API_KEY", "fixture-not-a-credential")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        return;
    }
    let directory = PathBuf::from(std::env::var_os("CONFER_TEST_QUESTIONS").unwrap());
    let workspace = directory.canonicalize().unwrap();
    let store = StateStore::new(directory.join("rooms.json"));
    store
        .mutate(|state| {
            let mut record = room("room-1", &workspace.to_string_lossy());
            record
                .seats
                .push(seat("codex-seat", "codex-worker", AgentKind::Codex));
            record
                .seats
                .push(seat("cursor-seat", "cursor-worker", AgentKind::Cursor));
            state.rooms.push(record);
            Ok(())
        })
        .unwrap();
    let server = ConferMcp::with_store(store.clone());
    for (seat_id, name) in [
        ("codex-seat", "codex-worker"),
        ("cursor-seat", "cursor-worker"),
    ] {
        let send = |message: &str| SendMessageArgs {
            workspace: workspace.clone(),
            room_id: "room-1".into(),
            recipients: vec![name.into()],
            message: message.into(),
        };
        let receipt = server
            .send_message_inner(send("ask a question"))
            .await
            .unwrap();
        let result = server
            .wait_output_inner(WaitOutputArgs {
                workspace: workspace.clone(),
                room_id: "room-1".into(),
                delivery_ids: vec![receipt.deliveries[0].delivery_id.clone()],
                timeout_ms: Some(5000),
            })
            .await
            .unwrap();
        assert!(result.completed, "{result:?}");
        let delivery = &result.deliveries[0];
        assert_eq!(
            delivery.status,
            DeliveryStatus::AwaitingAnswer,
            "{delivery:?}"
        );
        assert!(delivery.error.is_none(), "{delivery:?}");
        assert!(
            delivery
                .question
                .as_ref()
                .unwrap()
                .to_string()
                .contains("Which target?")
        );
        assert!(!store.seat_dispatch_uncertain("room-1", seat_id));
        let receipt = server
            .send_message_inner(send("host-answer: target A"))
            .await
            .unwrap();
        let result = server
            .wait_output_inner(WaitOutputArgs {
                workspace: workspace.clone(),
                room_id: "room-1".into(),
                delivery_ids: vec![receipt.deliveries[0].delivery_id.clone()],
                timeout_ms: Some(5000),
            })
            .await
            .unwrap();
        assert_eq!(
            result.deliveries[0].status,
            DeliveryStatus::Completed,
            "{result:?}"
        );
        assert_eq!(
            result.deliveries[0].final_answer.as_deref(),
            Some("answer received")
        );
        eprintln!(
            "{name}: awaiting_answer with question; native turn stopped; lease cleared; host answer resumed same native session"
        );
        let receipt = server
            .send_message_inner(send("failed-cleanup question"))
            .await
            .unwrap();
        let result = server
            .wait_output_inner(WaitOutputArgs {
                workspace: workspace.clone(),
                room_id: "room-1".into(),
                delivery_ids: vec![receipt.deliveries[0].delivery_id.clone()],
                timeout_ms: Some(7000),
            })
            .await
            .unwrap();
        assert!(result.completed, "{result:?}");
        assert!(result.deliveries[0].error.is_some(), "{result:?}");
        assert!(
            result.deliveries[0]
                .question
                .as_ref()
                .unwrap()
                .to_string()
                .contains("Which target?"),
            "{result:?}"
        );
        assert!(
            !std::fs::read_to_string(store.path())
                .unwrap()
                .contains("Which target?")
        );
    }
    let wire = std::fs::read_to_string(directory.join("wire")).unwrap();
    assert!(!wire.contains("\"answers\": {}"));
    assert!(!wire.contains("skipped"));
    assert!(!wire.contains("request_permission"));
}
