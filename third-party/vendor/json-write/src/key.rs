#[cfg(feature = "alloc")]
use alloc::borrow::Cow;
#[cfg(feature = "alloc")]
use alloc::string::String;

use crate::JsonWrite;

#[cfg(feature = "alloc")]
pub trait ToJsonKey {
    fn to_json_key(&self) -> String;
}

#[cfg(feature = "alloc")]
impl<T> ToJsonKey for T
where
    T: WriteJsonKey + ?Sized,
{
    fn to_json_key(&self) -> String {
        let mut result = String::new();
        let _ = self.write_json_key(&mut result);
        result
    }
}

pub trait WriteJsonKey {
    fn write_json_key<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result;
}

impl WriteJsonKey for str {
    fn write_json_key<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        crate::value::write_json_str(self, writer)
    }
}

#[cfg(feature = "alloc")]
impl WriteJsonKey for String {
    fn write_json_key<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        self.as_str().write_json_key(writer)
    }
}

#[cfg(feature = "alloc")]
impl WriteJsonKey for Cow<'_, str> {
    fn write_json_key<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        self.as_ref().write_json_key(writer)
    }
}

impl<V: WriteJsonKey + ?Sized> WriteJsonKey for &V {
    fn write_json_key<W: JsonWrite + ?Sized>(&self, writer: &mut W) -> core::fmt::Result {
        (*self).write_json_key(writer)
    }
}
