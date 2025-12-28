pub(crate) const FAILED: anstyle::Style =
    anstyle::Style::new().fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Red)));
pub(crate) const OK: anstyle::Style =
    anstyle::Style::new().fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Green)));
pub(crate) const IGNORED: anstyle::Style =
    anstyle::Style::new().fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Yellow)));
