// SPDX-License-Identifier: GPL-3.0-only
//! Event-driven logind lid gate. Closed lids suppress automatic color work.
use anyhow::{bail, Context, Result};
use futures_lite::{future::block_on, StreamExt};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, SyncSender},
        Arc,
    },
    thread,
};
use zbus::{Connection, Proxy};

fn publish(state: &AtomicBool, value: bool, wake: &SyncSender<()>) {
    if state.swap(value, Ordering::AcqRel) != value {
        eprintln!(
            "wallpaper-aura: lid {}; automatic colors {}",
            if value { "closed" } else { "open" },
            if value { "paused" } else { "resumed" }
        );
        let _ = wake.try_send(());
    }
}
pub(crate) fn watch(wake: SyncSender<()>, failed: Arc<AtomicBool>) -> Result<Arc<AtomicBool>> {
    let closed = Arc::new(AtomicBool::new(false));
    let state = closed.clone();
    let (ready, initial) = mpsc::sync_channel::<std::result::Result<(), String>>(1);
    thread::Builder::new()
        .name("lid-events".into())
        .spawn(move || {
            let result: Result<()> = block_on(async {
                let bus = Connection::system().await?;
                let proxy = Proxy::new(
                    &bus,
                    "org.freedesktop.login1",
                    "/org/freedesktop/login1",
                    "org.freedesktop.login1.Manager",
                )
                .await?;
                // Subscribe before reading the initial value, closing startup races.
                let mut changes = proxy.receive_property_changed::<bool>("LidClosed").await;
                let value: bool = proxy.get_property("LidClosed").await?;
                state.store(value, Ordering::Release);
                ready
                    .send(Ok(()))
                    .context("lid initial receiver disappeared")?;
                while let Some(change) = changes.next().await {
                    let value = change.get().await?;
                    publish(&state, value, &wake);
                }
                bail!("logind lid notification stream ended")
            });
            if let Err(error) = result {
                let _ = ready.try_send(Err(format!("{error:#}")));
                eprintln!("wallpaper-aura: lid watcher failed: {error:#}");
                failed.store(true, Ordering::Release);
                let _ = wake.try_send(());
            }
        })?;
    initial
        .recv()
        .context("lid watcher exited during startup")?
        .map_err(anyhow::Error::msg)?;
    Ok(closed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transitions_coalesce_without_losing_final_lid_state() {
        let state = AtomicBool::new(false);
        let (tx, rx) = mpsc::sync_channel(1);
        publish(&state, false, &tx);
        assert!(rx.try_recv().is_err());
        publish(&state, true, &tx);
        assert!(state.load(Ordering::Acquire));
        publish(&state, false, &tx);
        assert!(!state.load(Ordering::Acquire));
        rx.try_recv().unwrap();
        assert!(rx.try_recv().is_err());
    }
    #[test]
    #[ignore = "requires the host logind system bus; read-only"]
    fn live_lid_subscription() {
        let (tx, _rx) = mpsc::sync_channel(1);
        let failed = Arc::new(AtomicBool::new(false));
        let gate = watch(tx, failed.clone()).unwrap();
        assert!(!failed.load(Ordering::Acquire));
        eprintln!("live LidClosed={}", gate.load(Ordering::Acquire));
    }
}
