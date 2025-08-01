use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RueType {
    I32,
    I64,
    Bool,
    Unit,
    Unknown,
}

impl fmt::Display for RueType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RueType::I32 => write!(f, "i32"),
            RueType::I64 => write!(f, "i64"),
            RueType::Bool => write!(f, "bool"),
            RueType::Unit => write!(f, "()"),
            RueType::Unknown => write!(f, "unknown"),
        }
    }
}
