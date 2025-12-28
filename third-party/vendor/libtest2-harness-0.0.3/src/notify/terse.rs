use super::Event;
use super::MessageKind;
use super::FAILED;
use super::IGNORED;
use super::OK;

#[derive(Debug)]
pub(crate) struct TerseListNotifier<W> {
    writer: W,
    tests: usize,
}

impl<W: std::io::Write> TerseListNotifier<W> {
    pub(crate) fn new(writer: W) -> Self {
        Self { writer, tests: 0 }
    }
}

impl<W: std::io::Write> super::Notifier for TerseListNotifier<W> {
    fn notify(&mut self, event: Event) -> std::io::Result<()> {
        match event {
            Event::DiscoverStart(_) => {}
            Event::DiscoverCase(inner) => {
                if inner.selected {
                    let name = &inner.name;
                    let mode = inner.mode.as_str();
                    writeln!(self.writer, "{name}: {mode}")?;
                    self.tests += 1;
                }
            }
            Event::DiscoverComplete(_) => {
                writeln!(self.writer)?;
                writeln!(self.writer, "{} tests", self.tests)?;
                writeln!(self.writer)?;
            }
            Event::RunStart(_) => {}
            Event::CaseStart(_) => {}
            Event::CaseMessage(_) => {}
            Event::CaseComplete(_) => {}
            Event::RunComplete(_) => {}
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct TerseRunNotifier<W> {
    writer: W,
    summary: super::Summary,
}

impl<W: std::io::Write> TerseRunNotifier<W> {
    pub(crate) fn new(writer: W) -> Self {
        Self {
            writer,
            summary: Default::default(),
        }
    }
}

impl<W: std::io::Write> super::Notifier for TerseRunNotifier<W> {
    fn notify(&mut self, event: Event) -> std::io::Result<()> {
        self.summary.notify(event.clone())?;
        match event {
            Event::DiscoverStart(_) => {}
            Event::DiscoverCase(_) => {}
            Event::DiscoverComplete(_) => {}
            Event::RunStart(_) => {
                self.summary.write_start(&mut self.writer)?;
            }
            Event::CaseStart(_) => {}
            Event::CaseMessage(_) => {}
            Event::CaseComplete(inner) => {
                let status = self.summary.get_kind(&inner.name);
                let (c, style) = match status {
                    Some(MessageKind::Ignored) => ('i', IGNORED),
                    Some(MessageKind::Error) => ('F', FAILED),
                    None => ('.', OK),
                };
                write!(self.writer, "{style}{c}{style:#}")?;
                self.writer.flush()?;
            }
            Event::RunComplete(_) => {
                self.summary.write_complete(&mut self.writer)?;
            }
        }
        Ok(())
    }
}
