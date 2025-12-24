pub(crate) struct Style;

impl std::fmt::Display for Style {
    fn fmt(&self, _formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Ok(())
    }
}

pub(crate) const FAILED: Style = Style;
pub(crate) const OK: Style = Style;
pub(crate) const IGNORED: Style = Style;
