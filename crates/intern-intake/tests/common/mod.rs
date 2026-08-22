#![allow(dead_code)]

use std::{
    fs,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use intern_intake::{Clock, DocumentFacts, MachineIdentity};

/// Injectable clock so no test ever waits out a real lease.
pub struct MockClock(AtomicI64);

impl MockClock {
    pub fn at(start: i64) -> Arc<Self> {
        Arc::new(Self(AtomicI64::new(start)))
    }

    pub fn at_real_now() -> Arc<Self> {
        Self::at(real_now())
    }

    pub fn set(&self, value: i64) {
        self.0.store(value, Ordering::SeqCst);
    }

    pub fn advance(&self, seconds: i64) -> i64 {
        self.0.fetch_add(seconds, Ordering::SeqCst) + seconds
    }
}

impl Clock for MockClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

pub fn real_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

pub fn identity(id: &str, name: &str) -> MachineIdentity {
    MachineIdentity {
        id: id.to_string(),
        name: name.to_string(),
        user: "tester".to_string(),
    }
}

/// Stat-derived facts for a file that already exists under `root`.
pub fn facts_for(root: &Path, relative: &str) -> DocumentFacts {
    let path = root.join(relative);
    let metadata = fs::metadata(&path).unwrap();
    let modified_secs = metadata
        .modified()
        .unwrap()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    DocumentFacts {
        relative_path: relative.replace('\\', "/"),
        size: metadata.len(),
        modified_secs,
    }
}

/// Polls a condition without depending on scan timing; the watcher tests use
/// millisecond scan intervals so this returns almost immediately.
pub fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for: {what}");
}
