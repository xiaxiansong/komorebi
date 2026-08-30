use std::collections::HashMap;
use std::collections::hash_map::Keys;

use crate::windows_api::WindowsApi;

/// The identity a window handle carries while it is temporarily unmanaged.
///
/// A numeric window handle is not an identity. Windows reuses handles freely, so a handle held in
/// the suspension set can come to name a completely different window, and suppressing events for
/// that window would leave it unmanageable for the lifetime of the process with no command able to
/// recover it: the resume path refuses a window komorebi does not own.
///
/// The owning process is what makes the handle answerable. It is read once, when the window is
/// suspended, and compared whenever the entry is consulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuspendedWindow {
    pub hwnd: isize,
    /// The process which owned the window when it was suspended.
    ///
    /// `None` when Win32 would not say. Without an anchor there is nothing a later observation can
    /// contradict, so such an entry is only ever given up explicitly - by a destroy, a resume or a
    /// reap - which is exactly how the set behaved before identities existed.
    pub process_id: Option<u32>,
}

/// What a handle currently names, as far as the suspension set is concerned.
///
/// Injected rather than called directly so the reuse and death cases can be tested without a
/// desktop session.
pub trait WindowIdentity {
    /// The process which currently owns `hwnd`, or `None` when the handle names no window.
    fn identify(&self, hwnd: isize) -> Option<u32>;
}

/// The identity source used in a running window manager.
#[derive(Debug, Clone, Copy, Default)]
pub struct Win32Identity;

impl WindowIdentity for Win32Identity {
    fn identify(&self, hwnd: isize) -> Option<u32> {
        if !WindowsApi::is_window(hwnd) {
            return None;
        }

        let (process_id, _) = WindowsApi::window_thread_process_id(hwnd);

        Some(process_id)
    }
}

/// The windows which have been explicitly detached from management at runtime.
///
/// Membership is what stops an ordinary Win32 show, move or focus event from handing a suspended
/// window back to management. It is deliberately not persisted: a new window manager process
/// treats every still-existing, otherwise eligible handle as a newly opened window.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SuspensionSet {
    entries: HashMap<isize, SuspendedWindow>,
}

impl SuspensionSet {
    /// Suspend `hwnd`, recording the process which owns it now.
    pub fn insert(&mut self, hwnd: isize) {
        self.insert_with(hwnd, &Win32Identity);
    }

    pub fn insert_with(&mut self, hwnd: isize, identity: &impl WindowIdentity) {
        self.entries.insert(
            hwnd,
            SuspendedWindow {
                hwnd,
                process_id: identity.identify(hwnd),
            },
        );
    }

    /// Whether the set holds this handle, without asking Win32 anything.
    ///
    /// This is the plain membership question, used where a Win32 call would be wrong or wasteful:
    /// invariant validation, and reporting what the set holds.
    #[must_use]
    pub fn contains(&self, hwnd: isize) -> bool {
        self.entries.contains_key(&hwnd)
    }

    /// Whether the set still holds this handle *and* the handle still names the window it was
    /// suspended with.
    ///
    /// A handle which has died or been reused is dropped here and reported as unheld, which is
    /// what returns a reused handle to ordinary management. Nothing is asked of Win32 for a handle
    /// the set does not hold, so this costs nothing on the event path in the common case of an
    /// empty set.
    pub fn claims(&mut self, hwnd: isize) -> bool {
        self.claims_with(hwnd, &Win32Identity)
    }

    pub fn claims_with(&mut self, hwnd: isize, identity: &impl WindowIdentity) -> bool {
        let Some(entry) = self.entries.get(&hwnd) else {
            return false;
        };

        if Self::is_stale(*entry, identity) {
            tracing::info!(
                "reclaiming temporarily unmanaged hwnd {hwnd}: it no longer names the suspended window"
            );
            self.entries.remove(&hwnd);
            return false;
        }

        true
    }

    /// Stop suspending `hwnd`, reporting whether it was suspended at all.
    pub fn remove(&mut self, hwnd: isize) -> bool {
        self.entries.remove(&hwnd).is_some()
    }

    /// Drop every entry whose handle has died or been reused, reporting what was dropped.
    ///
    /// Called where a stale entry would change an answer rather than on every event: the resume
    /// subject heuristic counts entries, and the suspend and resume commands report on them.
    pub fn reclaim_stale(&mut self) -> Vec<isize> {
        self.reclaim_stale_with(&Win32Identity)
    }

    pub fn reclaim_stale_with(&mut self, identity: &impl WindowIdentity) -> Vec<isize> {
        let stale: Vec<isize> = self
            .entries
            .values()
            .filter(|entry| Self::is_stale(**entry, identity))
            .map(|entry| entry.hwnd)
            .collect();

        for hwnd in &stale {
            tracing::info!("reclaiming stale temporarily unmanaged hwnd {hwnd}");
            self.entries.remove(hwnd);
        }

        stale
    }

    /// Whether the handle has stopped naming the window which was suspended with it.
    ///
    /// One comparison covers both ways that happens. The handle names no window at all, because
    /// the application was closed or crashed without a destroy event ever reaching the model; or
    /// it names a window owned by a different process, because Windows has handed the number to
    /// something else.
    fn is_stale(entry: SuspendedWindow, identity: &impl WindowIdentity) -> bool {
        match entry.process_id {
            Some(suspended) => identity.identify(entry.hwnd) != Some(suspended),
            // Nothing was known about the window when it was suspended, so there is no anchor to
            // contradict and the entry is kept until something gives it up explicitly.
            None => false,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The suspended handles, in no particular order.
    pub fn hwnds(&self) -> Keys<'_, isize, SuspendedWindow> {
        self.entries.keys()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An identity source which answers from a table instead of from the desktop.
    #[derive(Default)]
    struct FakeIdentity {
        owners: HashMap<isize, u32>,
    }

    impl FakeIdentity {
        fn with(owners: &[(isize, u32)]) -> Self {
            Self {
                owners: owners.iter().copied().collect(),
            }
        }
    }

    impl WindowIdentity for FakeIdentity {
        fn identify(&self, hwnd: isize) -> Option<u32> {
            self.owners.get(&hwnd).copied()
        }
    }

    #[test]
    fn a_suspended_handle_is_claimed_while_it_names_the_same_window() {
        let identity = FakeIdentity::with(&[(42, 1000)]);
        let mut set = SuspensionSet::default();
        set.insert_with(42, &identity);

        assert!(set.claims_with(42, &identity));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn a_handle_the_set_never_held_is_not_claimed() {
        let identity = FakeIdentity::with(&[(42, 1000)]);
        let mut set = SuspensionSet::default();

        assert!(!set.claims_with(42, &identity));
    }

    #[test]
    fn a_reused_handle_is_reclaimed_rather_than_suppressed() {
        let identity = FakeIdentity::with(&[(42, 1000)]);
        let mut set = SuspensionSet::default();
        set.insert_with(42, &identity);

        // The suspended window closed and Windows handed the same number to another process.
        let reused = FakeIdentity::with(&[(42, 2000)]);

        assert!(!set.claims_with(42, &reused));
        assert!(!set.contains(42));
    }

    #[test]
    fn a_dead_handle_is_reclaimed() {
        let identity = FakeIdentity::with(&[(42, 1000)]);
        let mut set = SuspensionSet::default();
        set.insert_with(42, &identity);

        assert!(!set.claims_with(42, &FakeIdentity::default()));
        assert!(!set.contains(42));
    }

    #[test]
    fn an_entry_suspended_without_an_identity_is_never_reclaimed() {
        let mut set = SuspensionSet::default();
        set.insert_with(42, &FakeIdentity::default());

        // Nothing was known at suspension, so nothing observed later can contradict it: the entry
        // is given up by a destroy, a resume or a reap, not by a guess.
        assert!(set.claims_with(42, &FakeIdentity::with(&[(42, 2000)])));
        assert!(set.claims_with(42, &FakeIdentity::default()));
        assert!(set.reclaim_stale_with(&FakeIdentity::default()).is_empty());
    }

    #[test]
    fn pruning_drops_only_the_stale_entries() {
        let identity = FakeIdentity::with(&[(42, 1000), (43, 1001), (44, 1002)]);
        let mut set = SuspensionSet::default();

        for hwnd in [42, 43, 44] {
            set.insert_with(hwnd, &identity);
        }

        // 42 survives, 43 was closed, and 44 was handed to another process.
        let later = FakeIdentity::with(&[(42, 1000), (44, 9999)]);

        let mut reclaimed = set.reclaim_stale_with(&later);
        reclaimed.sort_unstable();

        assert_eq!(reclaimed, vec![43, 44]);
        assert_eq!(set.hwnds().copied().collect::<Vec<_>>(), vec![42]);
    }

    #[test]
    fn removal_reports_whether_the_handle_was_held() {
        let identity = FakeIdentity::with(&[(42, 1000)]);
        let mut set = SuspensionSet::default();
        set.insert_with(42, &identity);

        assert!(set.remove(42));
        assert!(!set.remove(42));
        assert!(set.is_empty());
    }
}
