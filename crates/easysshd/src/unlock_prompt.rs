//! The touch and PIN dialogs of the FIDO2 vault unlock, shown through the
//! same askpass hub as the ones for FIDO2 SSH keys (`security_key_touch`
//! notice, `security_key_pin` prompt), so every attached client already knows
//! how to display them.
//!
//! The vault asks from a blocking thread (a security key call waits for the
//! user), so the hub's async prompt is driven on a helper thread: that is
//! safe from any context, including a runtime worker.

use crate::askpass::Hub;
use easyssh_vault::UnlockPrompt;
use std::sync::Arc;
use tokio::runtime::Handle;

pub struct DaemonUnlockPrompt {
    hub: Arc<Hub>,
    runtime: Handle,
}

impl DaemonUnlockPrompt {
    pub fn new(hub: Arc<Hub>, runtime: Handle) -> Self {
        Self { hub, runtime }
    }
}

impl UnlockPrompt for DaemonUnlockPrompt {
    fn touch(&self, label: &str) -> Box<dyn Send> {
        let text = format!("Touch your security key ({label})");
        Box::new(
            self.hub
                .notice("AnixOps SSH vault", &text, "security_key_touch"),
        )
    }

    fn pin(&self, label: &str, retries: Option<u32>) -> Option<String> {
        let mut text = format!("Enter the PIN of your security key ({label})");
        if let Some(retries) = retries {
            text.push_str(&format!(" - {retries} tries left"));
        }
        let hub = self.hub.clone();
        let runtime = self.runtime.clone();
        std::thread::spawn(move || {
            runtime.block_on(hub.prompt("AnixOps SSH vault", &text, "security_key_pin", false))
        })
        .join()
        .ok()
        .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    type Shown = Arc<Mutex<Vec<(u64, String, Option<&'static str>)>>>;

    async fn hub() -> (Arc<Hub>, Shown, Arc<Mutex<Vec<u64>>>) {
        let shown: Shown = Arc::default();
        let closed: Arc<Mutex<Vec<u64>>> = Arc::default();
        let (seen, ended) = (shown.clone(), closed.clone());
        let hub = Hub::bind(
            Arc::new(move |p| {
                seen.lock().unwrap().push((p.id, p.prompt, p.kind));
                true
            }),
            Arc::new(move |id| ended.lock().unwrap().push(id)),
        )
        .await
        .unwrap();
        (hub, shown, closed)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_touch_notice_closes_when_the_guard_is_dropped() {
        let (hub, shown, closed) = hub().await;
        let prompt = DaemonUnlockPrompt::new(hub, Handle::current());
        let guard = prompt.touch("Desk key");
        let (id, text, kind) = shown.lock().unwrap()[0].clone();
        assert_eq!(kind, Some("security_key_touch"));
        assert!(text.contains("Desk key"), "{text}");
        assert!(closed.lock().unwrap().is_empty());
        drop(guard);
        assert_eq!(*closed.lock().unwrap(), vec![id]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_pin_is_asked_through_the_hub_even_from_a_runtime_thread() {
        let (hub, shown, _) = hub().await;
        let prompt = Arc::new(DaemonUnlockPrompt::new(hub.clone(), Handle::current()));
        let asking = prompt.clone();
        // Called straight from an async task, as the vault may be.
        let task = tokio::spawn(async move { asking.pin("Desk key", Some(3)) });
        let id = loop {
            let first = shown.lock().unwrap().first().cloned();
            if let Some((id, text, kind)) = first {
                assert_eq!(kind, Some("security_key_pin"));
                assert!(text.contains("3 tries left"), "{text}");
                break id;
            }
            tokio::task::yield_now().await;
        };
        hub.reply(id, Some("2468".into()), false);
        assert_eq!(task.await.unwrap().as_deref(), Some("2468"));
    }
}
