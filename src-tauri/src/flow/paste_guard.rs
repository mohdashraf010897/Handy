//! Atomic `[Paste raw now]` escape hatch and last-dictation rescue buffer.
//!
//! Coordinates ownership of an active LLM-polishing turn so clicking
//! `[Paste raw now]` while a slow or blocking-FFI provider is still running
//! pastes immediately and guarantees the late LLM result cannot double-paste.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use tokio::sync::Notify;

const MAX_RECENT_DICTATIONS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnWinner {
    Raw,
    Llm,
}

struct ActivePolishing {
    turn_id: u64,
    winner: Option<TurnWinner>,
    notify: Arc<Notify>,
}

struct PolishingState {
    next_turn_id: u64,
    active: Option<ActivePolishing>,
}

impl PolishingState {
    const fn new() -> Self {
        Self {
            next_turn_id: 1,
            active: None,
        }
    }
}

struct RecentDictations {
    next_id: u32,
    entries: VecDeque<(u32, String)>,
}

impl RecentDictations {
    const fn new() -> Self {
        Self {
            next_id: 1,
            entries: VecDeque::new(),
        }
    }
}

static STATE: Mutex<PolishingState> = Mutex::new(PolishingState::new());
static RECENT_DICTATIONS: Mutex<RecentDictations> = Mutex::new(RecentDictations::new());

#[cfg(test)]
static TEST_LOCK: Mutex<()> = Mutex::new(());

/// Starts tracking an in-flight LLM polishing turn (`turn_id >= 1`).
pub fn begin_polishing() -> (u64, Arc<Notify>) {
    let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
    let turn_id = state.next_turn_id;
    state.next_turn_id = state.next_turn_id.saturating_add(1);
    let notify = Arc::new(Notify::new());
    state.active = Some(ActivePolishing {
        turn_id,
        winner: None,
        notify: Arc::clone(&notify),
    });
    (turn_id, notify)
}

/// Atomically locks in `TurnWinner::Raw` for the active unclaimed polishing
/// turn and wakes waiters. Returns `true` at most once per active turn.
pub fn request_paste_raw_now() -> bool {
    let notify = {
        let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(active) = state.active.as_mut() else {
            return false;
        };
        if active.winner.is_some() {
            return false;
        }
        active.winner = Some(TurnWinner::Raw);
        Arc::clone(&active.notify)
    };
    notify.notify_waiters();
    true
}

/// Returns whether raw paste has already won `turn_id`.
pub fn is_skip_requested(turn_id: u64) -> bool {
    let state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
    state
        .active
        .as_ref()
        .is_some_and(|active| active.turn_id == turn_id && active.winner == Some(TurnWinner::Raw))
}

/// Atomically resolves `turn_id` and removes the active turn.
/// If `request_paste_raw_now` already locked in `TurnWinner::Raw`, `Raw` wins
/// even if the LLM finished microseconds earlier; otherwise `prefer` wins.
pub fn resolve_polishing(turn_id: u64, prefer: TurnWinner) -> Option<TurnWinner> {
    let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
    let active = state.active.as_ref()?;
    if active.turn_id != turn_id {
        return None;
    }
    let winner = active.winner.unwrap_or(prefer);
    state.active = None;
    Some(winner)
}

/// Clears `turn_id` if it is still the active polishing turn (e.g. cancelled).
pub fn cancel_polishing(turn_id: u64) {
    let mut state = STATE.lock().unwrap_or_else(PoisonError::into_inner);
    if state
        .active
        .as_ref()
        .is_some_and(|active| active.turn_id == turn_id)
    {
        state.active = None;
    }
}

/// Stores a non-empty final dictation text in a bounded ring buffer for paste
/// rescue, returning its unique `dictation_id` so failure toasts stay bound to
/// the exact dictation that failed to paste.
pub fn remember_last_dictation(text: &str) -> Option<u32> {
    if text.trim().is_empty() {
        return None;
    }
    let mut recent = RECENT_DICTATIONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let id = recent.next_id;
    recent.next_id = if recent.next_id == u32::MAX {
        1
    } else {
        recent.next_id + 1
    };
    if recent.entries.len() >= MAX_RECENT_DICTATIONS {
        recent.entries.pop_front();
    }
    recent.entries.push_back((id, text.to_string()));
    Some(id)
}

/// Clones the requested dictation by `id` (if given), or the most recent one.
pub fn get_dictation(dictation_id: Option<u32>) -> Option<String> {
    let recent = RECENT_DICTATIONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    match dictation_id {
        Some(target_id) => recent
            .entries
            .iter()
            .rev()
            .find(|(id, _)| *id == target_id)
            .map(|(_, text)| text.clone()),
        None => recent.entries.back().map(|(_, text)| text.clone()),
    }
}

/// Clones the latest remembered non-empty dictation text, if any.
pub fn get_last_dictation() -> Option<String> {
    get_dictation(None)
}

/// Returns the ID of the most recently recorded dictation, if any.
pub fn latest_dictation_id() -> Option<u32> {
    RECENT_DICTATIONS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .entries
        .back()
        .map(|(id, _)| *id)
}

#[cfg(test)]
mod tests {
    use std::sync::Barrier;
    use std::thread;

    use super::*;

    fn reset_for_test() {
        STATE.lock().unwrap_or_else(PoisonError::into_inner).active = None;
        RECENT_DICTATIONS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entries
            .clear();
    }

    #[test]
    fn normal_llm_finish_claims_once_and_blocks_subsequent_paste_raw() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        reset_for_test();

        let (turn_id, _notify) = begin_polishing();
        assert!(turn_id >= 1);
        assert!(!is_skip_requested(turn_id));
        assert_eq!(
            resolve_polishing(turn_id, TurnWinner::Llm),
            Some(TurnWinner::Llm)
        );
        assert_eq!(
            resolve_polishing(turn_id, TurnWinner::Llm),
            None,
            "second resolution on same turn must fail"
        );
        assert!(
            !request_paste_raw_now(),
            "raw paste after LLM resolved turn must return false"
        );
    }

    #[test]
    fn paste_raw_wins_even_if_llm_tries_to_resolve_after_raw_request() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        reset_for_test();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        runtime.block_on(async {
            let (turn_id, notify) = begin_polishing();
            let waiter = tokio::spawn(async move {
                let notified = notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if !is_skip_requested(turn_id) {
                    notified.await;
                }
                // Even if caller passes Llm preference, Raw already locked in.
                resolve_polishing(turn_id, TurnWinner::Llm)
            });

            tokio::task::yield_now().await;
            assert!(request_paste_raw_now());

            let winner = waiter.await.expect("waiter completed");
            assert_eq!(winner, Some(TurnWinner::Raw));
            assert_eq!(
                resolve_polishing(turn_id, TurnWinner::Llm),
                None,
                "late LLM finish must not resolve already-claimed turn"
            );
        });
    }

    #[test]
    fn concurrent_request_paste_raw_now_succeeds_only_once() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        reset_for_test();

        let (turn_id, _notify) = begin_polishing();
        let workers = 8;
        let barrier = Arc::new(Barrier::new(workers));
        let mut handles = Vec::with_capacity(workers);

        for _ in 0..workers {
            let b = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                b.wait();
                request_paste_raw_now()
            }));
        }

        let successes = handles
            .into_iter()
            .map(|h| h.join().expect("thread"))
            .filter(|&ok| ok)
            .count();
        assert_eq!(successes, 1);
        assert!(is_skip_requested(turn_id));
        cancel_polishing(turn_id);
        assert_eq!(resolve_polishing(turn_id, TurnWinner::Raw), None);
    }

    #[test]
    fn remember_last_dictation_binds_ids_and_keeps_latest_text() {
        let _guard = TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        reset_for_test();

        assert_eq!(get_last_dictation(), None);
        assert_eq!(remember_last_dictation(""), None);
        assert_eq!(remember_last_dictation("   \n\t  "), None);
        assert_eq!(get_last_dictation(), None);

        let first_id = remember_last_dictation("First rescued dictation").expect("id");
        assert_eq!(
            get_last_dictation().as_deref(),
            Some("First rescued dictation")
        );

        assert_eq!(remember_last_dictation("   "), None);
        let second_id = remember_last_dictation("Updated dictation").expect("id");
        assert_eq!(get_last_dictation().as_deref(), Some("Updated dictation"));
        assert_eq!(
            get_dictation(Some(first_id)).as_deref(),
            Some("First rescued dictation"),
            "older toast id still retrieves the exact failed dictation"
        );
        assert_eq!(
            get_dictation(Some(second_id)).as_deref(),
            Some("Updated dictation")
        );
    }
}
