mod json;
#[cfg(not(feature = "color"))]
mod no_style;
mod pretty;
#[cfg(feature = "color")]
mod style;
mod summary;
mod terse;

pub(crate) use json::*;
#[cfg(not(feature = "color"))]
pub(crate) use no_style::*;
pub(crate) use pretty::*;
#[cfg(feature = "color")]
pub(crate) use style::*;
pub(crate) use summary::*;
pub(crate) use terse::*;

pub(crate) trait Notifier {
    fn threaded(&mut self, _yes: bool) {}

    fn notify(&mut self, event: Event) -> std::io::Result<()>;
}

#[derive(Clone)]
pub(crate) struct ArcNotifier {
    inner: std::sync::Arc<std::sync::Mutex<dyn Notifier + Send>>,
}

impl ArcNotifier {
    pub(crate) fn new(inner: impl Notifier + Send + 'static) -> Self {
        Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(inner)),
        }
    }

    pub(crate) fn threaded(&self, yes: bool) {
        let mut notifier = match self.inner.lock() {
            Ok(notifier) => notifier,
            Err(poison) => poison.into_inner(),
        };
        notifier.threaded(yes);
    }

    pub(crate) fn notify(&self, event: Event) -> std::io::Result<()> {
        let mut notifier = match self.inner.lock() {
            Ok(notifier) => notifier,
            Err(poison) => poison.into_inner(),
        };
        notifier.notify(event)
    }
}

pub(crate) use libtest_json::*;

pub use libtest_json::RunMode;
