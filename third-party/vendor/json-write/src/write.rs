pub trait JsonWrite: core::fmt::Write {
    fn open_object(&mut self) -> core::fmt::Result {
        write!(self, "{{")
    }
    fn close_object(&mut self) -> core::fmt::Result {
        write!(self, "}}")
    }

    fn open_array(&mut self) -> core::fmt::Result {
        write!(self, "[")
    }
    fn close_array(&mut self) -> core::fmt::Result {
        write!(self, "]")
    }

    fn keyval_sep(&mut self) -> core::fmt::Result {
        write!(self, ":")
    }

    /// Write an encoded JSON key
    fn key(&mut self, value: impl crate::WriteJsonKey) -> core::fmt::Result {
        value.write_json_key(self)
    }

    /// Write an encoded JSON scalar value
    ///
    /// <div class="warning">
    ///
    /// For floats, this preserves the sign bit for [`f32::NAN`] / [`f64::NAN`] for the sake of
    /// format-preserving editing.
    /// However, in most cases the sign bit is indeterminate and outputting signed NANs can be a
    /// cause of non-repeatable behavior.
    ///
    /// For general serialization, you should discard the sign bit.  For example:
    /// ```
    /// # let mut v = f64::NAN;
    /// if v.is_nan() {
    ///     v = v.copysign(1.0);
    /// }
    /// ```
    ///
    /// </div>
    fn value(&mut self, value: impl crate::WriteJsonValue) -> core::fmt::Result {
        value.write_json_value(self)
    }

    fn val_sep(&mut self) -> core::fmt::Result {
        write!(self, ",")
    }

    fn space(&mut self) -> core::fmt::Result {
        write!(self, " ")
    }

    fn newline(&mut self) -> core::fmt::Result {
        writeln!(self)
    }
}

impl<W> JsonWrite for W where W: core::fmt::Write {}
