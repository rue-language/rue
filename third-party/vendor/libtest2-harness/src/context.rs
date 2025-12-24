pub(crate) use crate::*;

pub struct TestContext {
    pub(crate) start: std::time::Instant,
    pub(crate) mode: RunMode,
    pub(crate) run_ignored: bool,
    pub(crate) notifier: notify::ArcNotifier,
    pub(crate) test_name: String,
}

impl TestContext {
    pub fn ignore(&self) -> Result<(), RunError> {
        if self.run_ignored {
            Ok(())
        } else {
            Err(RunError::ignore())
        }
    }

    pub fn ignore_for(&self, reason: impl std::fmt::Display) -> Result<(), RunError> {
        if self.run_ignored {
            Ok(())
        } else {
            Err(RunError::ignore_for(reason.to_string()))
        }
    }

    pub fn current_mode(&self) -> RunMode {
        self.mode
    }

    pub fn notify(&self, event: notify::Event) -> std::io::Result<()> {
        self.notifier().notify(event)
    }

    pub fn elapased_s(&self) -> notify::Elapsed {
        notify::Elapsed(self.start.elapsed())
    }

    pub fn test_name(&self) -> &str {
        &self.test_name
    }

    pub(crate) fn notifier(&self) -> &notify::ArcNotifier {
        &self.notifier
    }

    pub(crate) fn clone(&self) -> Self {
        Self {
            start: self.start,
            mode: self.mode,
            run_ignored: self.run_ignored,
            notifier: self.notifier.clone(),
            test_name: self.test_name.clone(),
        }
    }
}
