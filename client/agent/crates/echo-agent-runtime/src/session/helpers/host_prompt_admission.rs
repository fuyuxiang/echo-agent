//! In-process desktop prompt admission. The host can withdraw a request
//! until the Runtime has actually queued it on the session actor.

use agent_client_protocol as acp;
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use uuid::Uuid;

const TICKET_KEY: &str = "echo.agent/desktopPromptAdmission";
// Valid only for an IPC submission the desktop renderer has already posted.
const IPC_RACE_WINDOW: Duration = Duration::from_secs(300);
type PromptKey = (String, String);

#[derive(Default)]
struct Admissions {
    active: HashMap<PromptKey, (Uuid, Phase)>,
    early_withdrawals: HashMap<PromptKey, Instant>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Preparing,
    Withdrawn,
    Dispatched,
}

fn admissions() -> &'static Mutex<Admissions> {
    static ADMISSIONS: OnceLock<Mutex<Admissions>> = OnceLock::new();
    ADMISSIONS.get_or_init(|| Mutex::new(Admissions::default()))
}

pub struct HostPromptAdmission {
    key: PromptKey,
    ticket: Uuid,
}

impl HostPromptAdmission {
    pub fn begin(session_id: &str, prompt_id: &str) -> Result<Self, String> {
        let key = (session_id.to_owned(), prompt_id.to_owned());
        let mut state = admissions().lock().unwrap();
        if state.active.contains_key(&key) {
            return Err("该消息正在处理，请等待当前发送结束".into());
        }
        let ticket = Uuid::new_v4();
        let phase = if state
            .early_withdrawals
            .remove(&key)
            .is_some_and(|time| time.elapsed() < IPC_RACE_WINDOW)
        {
            Phase::Withdrawn
        } else {
            Phase::Preparing
        };
        state.active.insert(key.clone(), (ticket, phase));
        Ok(Self { key, ticket })
    }

    pub fn withdrawn(&self) -> bool {
        admissions()
            .lock()
            .unwrap()
            .active
            .get(&self.key)
            .is_some_and(|(_, phase)| *phase == Phase::Withdrawn)
    }

    pub fn attach(&self, request: &mut acp::PromptRequest) {
        request.meta.get_or_insert_with(Default::default).insert(
            TICKET_KEY.into(),
            serde_json::json!(self.ticket.to_string()),
        );
    }
}

impl Drop for HostPromptAdmission {
    fn drop(&mut self) {
        admissions().lock().unwrap().active.remove(&self.key);
    }
}

/// Return true only if this exact prompt was prevented from reaching the actor.
/// A posted desktop IPC can be polled after its cancel IPC on the host pool.
pub fn withdraw(session_id: &str, prompt_id: &str, ipc_posted: bool) -> bool {
    let key = (session_id.to_owned(), prompt_id.to_owned());
    let mut state = admissions().lock().unwrap();
    if let Some((_, phase)) = state.active.get_mut(&key) {
        if *phase == Phase::Dispatched {
            return false;
        }
        *phase = Phase::Withdrawn;
        return true;
    }
    if ipc_posted {
        state
            .early_withdrawals
            .retain(|_, time| time.elapsed() < IPC_RACE_WINDOW);
        state.early_withdrawals.insert(key, Instant::now());
    }
    ipc_posted
}

pub fn withdraw_session(session_id: &str) {
    for ((session, _), (_, phase)) in &mut admissions().lock().unwrap().active {
        if session == session_id && *phase == Phase::Preparing {
            *phase = Phase::Withdrawn;
        }
    }
}

/// Check the ticket under the same lock as withdraw, through the synchronous
/// actor enqueue. The caller also holds Runtime's normal session dispatch
/// lock, so a cancel that loses this race follows the prompt to that actor.
pub(crate) fn dispatch<T>(
    session_id: &str,
    prompt_id: &str,
    meta: Option<&acp::Meta>,
    enqueue: impl FnOnce() -> T,
) -> Result<T, acp::Error> {
    let Some(ticket) = meta.and_then(|meta| meta.get(TICKET_KEY)) else {
        return Ok(enqueue());
    };
    let mut state = admissions().lock().unwrap();
    let key = (session_id.to_owned(), prompt_id.to_owned());
    let entry = state.active.get_mut(&key).filter(|(expected, phase)| {
        *phase == Phase::Preparing
            && ticket.as_str().and_then(|id| Uuid::parse_str(id).ok()) == Some(*expected)
    });
    let Some((_, phase)) = entry else {
        return Err(acp::Error::invalid_params().data("desktop prompt withdrawn before admission"));
    };
    *phase = Phase::Dispatched;
    Ok(enqueue())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(admission: &HostPromptAdmission) -> acp::PromptRequest {
        let mut request =
            acp::PromptRequest::new(acp::SessionId::new(admission.key.0.clone()), Vec::new());
        admission.attach(&mut request);
        request
    }

    #[test]
    fn withdrawn_preparation_cannot_enqueue_and_a_new_turn_still_can() {
        let session = "withdraw-before-actor";
        let old = HostPromptAdmission::begin(session, "old").unwrap();
        let prompt = request(&old);
        assert!(withdraw(session, "old", false));
        assert!(old.withdrawn());
        assert!(
            dispatch(session, "old", prompt.meta.as_ref(), || panic!(
                "old prompt dispatched"
            ))
            .is_err()
        );
        let new = HostPromptAdmission::begin(session, "new").unwrap();
        assert!(dispatch(session, "new", request(&new).meta.as_ref(), || 1).is_ok());
        assert!(!withdraw(session, "new", true));
    }

    #[test]
    fn exact_cancel_ipc_can_win_before_the_send_handler_is_polled() {
        let session = "cancel-ipc-first";
        assert!(!withdraw(session, "untracked", false));
        assert!(withdraw(session, "optimistic", true));
        let admission = HostPromptAdmission::begin(session, "optimistic").unwrap();
        assert!(admission.withdrawn());
        assert!(
            dispatch(
                session,
                "optimistic",
                request(&admission).meta.as_ref(),
                || panic!("withdrawn prompt dispatched")
            )
            .is_err()
        );
    }
}
