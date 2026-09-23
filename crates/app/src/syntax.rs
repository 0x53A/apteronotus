//! Debounced syntax feedback, independent of Run and of the audio worker.
use apteronotus_lua::{SyntaxDiagnostic, check_syntax};

const DEBOUNCE_SECONDS: f64 = 0.25;

pub struct SyntaxState {
    source: String,
    due_at: f64,
    submitted: bool,
    pub diagnostic: Option<SyntaxDiagnostic>,
    #[cfg(not(target_arch = "wasm32"))]
    worker: Worker,
}

impl Default for SyntaxState {
    fn default() -> Self {
        Self {
            source: String::new(),
            due_at: 0.0,
            submitted: false,
            diagnostic: None,
            #[cfg(not(target_arch = "wasm32"))]
            worker: Worker::new(),
        }
    }
}

impl SyntaxState {
    pub fn observe(&mut self, source: &str, now: f64) {
        if self.source != source {
            self.source.clear();
            self.source.push_str(source);
            self.due_at = now + DEBOUNCE_SECONDS;
            self.submitted = false;
            self.diagnostic = None;
        }
    }

    fn accept(&mut self, source: &str, diagnostic: Option<SyntaxDiagnostic>) {
        // Diagnostics derive from exact source, so undoing to a checked source
        // can safely reuse its result. A different document can never inherit it.
        if source == self.source {
            self.diagnostic = diagnostic;
        }
    }

    pub fn update(&mut self, source: &str, now: f64) {
        self.observe(source, now);
        #[cfg(not(target_arch = "wasm32"))]
        while let Ok((source, diagnostic)) = self.worker.results.try_recv() {
            self.accept(&source, diagnostic);
        }
        if !self.submitted && now >= self.due_at {
            #[cfg(not(target_arch = "wasm32"))]
            {
                self.submitted = self
                    .worker
                    .requests
                    .as_ref()
                    .is_some_and(|requests| requests.try_send(self.source.clone()).is_ok());
            }
            #[cfg(target_arch = "wasm32")]
            {
                let diagnostic = check_syntax(source).err();
                self.accept(source, diagnostic);
                self.submitted = true;
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct Worker {
    requests: Option<std::sync::mpsc::SyncSender<String>>,
    results: std::sync::mpsc::Receiver<(String, Option<SyntaxDiagnostic>)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Worker {
    fn new() -> Self {
        // One in flight and one queued, never a backlog of typed documents.
        let (requests, incoming) = std::sync::mpsc::sync_channel::<String>(1);
        let (outgoing, results) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            while let Ok(mut source) = incoming.recv() {
                while let Ok(newer) = incoming.try_recv() {
                    source = newer;
                }
                let diagnostic = check_syntax(&source).err();
                if outgoing.send((source, diagnostic)).is_err() {
                    break;
                }
            }
        });
        Self {
            requests: Some(requests),
            results,
            thread: Some(thread),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for Worker {
    fn drop(&mut self) {
        self.requests.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_edit_clears_diagnostics_and_restarts_the_debounce() {
        let mut state = SyntaxState::default();
        state.observe("local x = )", 1.0);
        state.accept("local x = )", check_syntax("local x = )").err());
        assert!(state.diagnostic.is_some());
        state.observe("local x = 2", 1.1);
        assert!(state.diagnostic.is_none());
        assert_eq!(state.due_at, 1.35);
        assert!(!state.submitted);
        state.observe("local x = 2", 1.2);
        assert_eq!(state.due_at, 1.35);
        state.accept("local x = )", check_syntax("local x = )").err());
        assert!(state.diagnostic.is_none());
    }
}
