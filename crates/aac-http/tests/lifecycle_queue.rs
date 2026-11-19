//! Exercise the Rust provider queue with an occupied workflow lease.
#![cfg(unix)]

use aac_http::lifecycle_queue::LifecycleQueues;
use std::{
    fs,
    future::{Future, poll_fn},
    os::unix::fs::PermissionsExt,
    pin::Pin,
    sync::Arc,
    task::Poll,
};
use tokio::sync::oneshot;

async fn pending(mut future: Pin<&mut impl Future>) -> bool {
    poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await
}

#[tokio::test]
async fn a_native_workflow_holds_its_provider_turn_until_it_settles()
-> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let path = root.path().canonicalize()?;
    let run = path.join("run");
    fs::create_dir(&run)?;
    fs::set_permissions(&run, fs::Permissions::from_mode(0o700))?;
    let queues = Arc::new(LifecycleQueues::new(path));
    let (occupied, first_started) = oneshot::channel();
    let (release, finished) = oneshot::channel();
    let owner = Arc::clone(&queues);
    let first = tokio::spawn(async move {
        owner
            .run("key:zai", async {
                let _ = occupied.send(());
                let _ = finished.await;
                Err::<(), _>("fixture rollback")
            })
            .await
    });
    first_started.await?;
    let second = queues.run("key:zai", async { 2 });
    let third = queues.run("key:zai", async { 3 });
    tokio::pin!(second, third);
    assert!(pending(second.as_mut()).await);
    assert!(pending(third.as_mut()).await);
    assert_eq!(queues.run("key:kimi-code", async { 1 }).await?, 1);

    assert!(release.send(()).is_ok());
    assert_eq!(first.await??, Err("fixture rollback"));
    assert!(pending(third.as_mut()).await);
    assert_eq!(second.await?, 2);
    assert_eq!(third.await?, 3);
    Ok(())
}
