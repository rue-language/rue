use super::Event;
use super::MessageKind;
use super::FAILED;
use super::IGNORED;
use super::OK;

#[derive(Debug)]
pub(crate) struct PrettyRunNotifier<W> {
    writer: W,
    is_multithreaded: bool,
    summary: super::Summary,
    name_width: usize,
}

impl<W: std::io::Write> PrettyRunNotifier<W> {
    pub(crate) fn new(writer: W) -> Self {
        Self {
            writer,
            is_multithreaded: false,
            summary: Default::default(),
            name_width: 0,
        }
    }
}

impl<W: std::io::Write> super::Notifier for PrettyRunNotifier<W> {
    fn threaded(&mut self, yes: bool) {
        self.is_multithreaded = yes;
    }

    fn notify(&mut self, event: Event) -> std::io::Result<()> {
        self.summary.notify(event.clone())?;
        match event {
            Event::DiscoverStart(_) => {}
            Event::DiscoverCase(inner) => {
                if inner.selected {
                    self.name_width = inner.name.len().max(self.name_width);
                }
            }
            Event::DiscoverComplete(_) => {}
            Event::RunStart(_) => {
                self.summary.write_start(&mut self.writer)?;
            }
            Event::CaseStart(inner) => {
                if !self.is_multithreaded {
                    write!(
                        self.writer,
                        "test {: <1$} ... ",
                        inner.name, self.name_width
                    )?;
                    self.writer.flush()?;
                }
            }
            Event::CaseMessage(_) => {}
            Event::CaseComplete(inner) => {
                let status = self.summary.get_kind(&inner.name);
                let (s, style) = match status {
                    Some(MessageKind::Ignored) => ("ignored", IGNORED),
                    Some(MessageKind::Error) => ("FAILED", FAILED),
                    None => ("ok", OK),
                };

                if self.is_multithreaded {
                    write!(
                        self.writer,
                        "test {: <1$} ... ",
                        inner.name, self.name_width
                    )?;
                }
                writeln!(self.writer, "{style}{s}{style:#}")?;
            }
            Event::RunComplete(_) => {
                self.summary.write_complete(&mut self.writer)?;
            }
        }
        Ok(())
    }
}
