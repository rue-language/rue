mod alignment;
mod archive;
mod archive_writer;

pub use archive::ArchiveKind;
pub use archive_writer::{get_native_object_symbols, write_archive_to_stream, NewArchiveMember};
