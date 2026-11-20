use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::watch;

type CreationResult = Option<Result<String, String>>;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct SessionCreationKey {
    pub cwd: String,
    pub project_id: Option<String>,
    pub profile_key: Option<String>,
    pub model_id: Option<String>,
    pub permission_mode: String,
}

#[derive(Clone)]
pub(crate) struct SessionCreationTicket {
    pub id: uuid::Uuid,
    result: watch::Receiver<CreationResult>,
}

impl SessionCreationTicket {
    pub async fn wait(&mut self) -> Result<String, String> {
        loop {
            if let Some(result) = self.result.borrow().clone() {
                return result;
            }
            self.result
                .changed()
                .await
                .map_err(|_| "Agent Runtime 未返回会话创建结果，请重试".to_string())?;
        }
    }
}

#[derive(Default)]
pub(crate) struct SessionCreations(Mutex<HashMap<SessionCreationKey, SessionCreationTicket>>);

impl SessionCreations {
    /// Timed-out callers leave their request here so an immediate retry waits
    /// for the same native creation instead of starting a second one.
    pub fn join(
        &self,
        key: &SessionCreationKey,
    ) -> Result<(SessionCreationTicket, Option<watch::Sender<CreationResult>>), String> {
        let mut pending = self.0.lock().map_err(|_| "会话创建状态已损坏")?;
        if let Some(ticket) = pending.get(key) {
            return Ok((ticket.clone(), None));
        }
        if pending.len() >= 128 {
            return Err("等待创建的会话过多，请稍后重试".into());
        }
        let (sender, result) = watch::channel(None);
        let ticket = SessionCreationTicket {
            id: uuid::Uuid::now_v7(),
            result,
        };
        pending.insert(key.clone(), ticket.clone());
        Ok((ticket, Some(sender)))
    }

    /// Only the matching Runtime request may claim/remove an entry. A late
    /// result from a retired generation cannot remove its replacement.
    pub fn finish(&self, key: &SessionCreationKey, id: uuid::Uuid) -> bool {
        let mut pending = self.0.lock().unwrap();
        if pending.get(key).is_some_and(|ticket| ticket.id == id) {
            pending.remove(key);
            true
        } else {
            false
        }
    }

    pub fn clear(&self) {
        self.0.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SessionCreationKey {
        SessionCreationKey {
            cwd: "/task".into(),
            project_id: None,
            profile_key: None,
            model_id: Some("model".into()),
            permission_mode: "ask".into(),
        }
    }

    #[tokio::test]
    async fn retry_before_creation_finishes_receives_the_same_session() {
        let requests = SessionCreations::default();
        let (first, sender) = requests.join(&key()).unwrap();
        let id = first.id;
        drop(first); // The original IPC stopped waiting.
        let (mut retry, second_sender) = requests.join(&key()).unwrap();
        assert!(
            second_sender.is_none(),
            "a retry must not start another Runtime request"
        );
        assert_eq!(retry.id, id);
        sender
            .unwrap()
            .send_replace(Some(Ok("created-once".into())));
        assert_eq!(retry.wait().await.unwrap(), "created-once");
        assert!(requests.finish(&key(), id));
        let (next, sender) = requests.join(&key()).unwrap();
        assert!(sender.is_some(), "a later new task needs its own request");
        assert_ne!(next.id, id);
    }

    #[tokio::test]
    async fn failures_are_delivered_and_retired_requests_cannot_claim_new_work() {
        let requests = SessionCreations::default();
        let (mut failed, sender) = requests.join(&key()).unwrap();
        sender
            .unwrap()
            .send_replace(Some(Err("creation failed".into())));
        assert_eq!(failed.wait().await.unwrap_err(), "creation failed");
        requests.clear();
        let (current, _) = requests.join(&key()).unwrap();
        assert!(!requests.finish(&key(), failed.id));
        assert!(requests.finish(&key(), current.id));
    }

    #[test]
    fn different_workspaces_models_and_permissions_never_share_requests() {
        let requests = SessionCreations::default();
        let (first, _) = requests.join(&key()).unwrap();
        for changed in [
            SessionCreationKey {
                profile_key: Some("different-expert-version".into()),
                ..key()
            },
            SessionCreationKey {
                cwd: "/other".into(),
                ..key()
            },
            SessionCreationKey {
                model_id: Some("other".into()),
                ..key()
            },
            SessionCreationKey {
                permission_mode: "auto".into(),
                ..key()
            },
        ] {
            let (ticket, sender) = requests.join(&changed).unwrap();
            assert!(sender.is_some());
            assert_ne!(ticket.id, first.id);
        }
    }
}
