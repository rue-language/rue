use super::Event;

#[derive(Debug)]
pub(crate) struct JsonNotifier<W> {
    writer: W,
}

impl<W: std::io::Write> JsonNotifier<W> {
    pub(crate) fn new(writer: W) -> Self {
        Self { writer }
    }
}

impl<W: std::io::Write> super::Notifier for JsonNotifier<W> {
    fn notify(&mut self, event: Event) -> std::io::Result<()> {
        let event = event.to_jsonline();
        writeln!(self.writer, "{event}")?;
        Ok(())
    }
}
