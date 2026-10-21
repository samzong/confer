use super::*;
use std::time::Duration;

async fn wait_for_zombie(id: i32) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if process_table()
                .await
                .unwrap()
                .iter()
                .any(|row| row.id == id && row.zombie)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn termination_and_cancellation_accept_exited_agents() {
    for cancel in [false, true] {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let id = child.id().unwrap();
        wait_for_zombie(id as i32).await;
        let cancellation = super::super::Cancellation::default();
        cancellation.set_process_id(id);
        if cancel {
            kill_target(-(id as i32)).await.unwrap();
            kill_target(id as i32).await.unwrap();
            cancellation.cancel();
            stop_cancelled_descendants(&cancellation).await;
            assert!(
                reap_with_cancellation(&mut child, &cancellation)
                    .await
                    .unwrap()
                    .unwrap()
                    .success()
            );
        } else {
            terminate(&mut child).await.unwrap();
            assert!(wait_after_kill(&mut child).await.unwrap().success());
        }
        assert!(!cancellation.is_uncertain());
    }
}

#[tokio::test]
async fn termination_cleans_live_group_after_leader_exit() {
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("late-write");
    let mut child = Command::new("python3")
        .args(["-c", r#"
import subprocess,sys
subprocess.Popen([sys.executable,'-c',"import sys,time; time.sleep(1); open(sys.argv[1],'w').write('leaked')",sys.argv[1]],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
"#])
        .arg(&marker)
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let id = child.id().unwrap();
    wait_for_zombie(id as i32).await;
    assert!(
        process_table()
            .await
            .unwrap()
            .iter()
            .any(|row| row.group == id as i32 && !row.zombie)
    );
    terminate(&mut child).await.unwrap();
    assert!(wait_after_kill(&mut child).await.unwrap().success());
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert!(!marker.exists());
}
