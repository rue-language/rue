//! MIR optimization passes

pub mod const_prop;
pub mod cse;
pub mod dead_code;

pub use const_prop::ConstProp;
pub use cse::CommonSubexpressionElimination;
pub use dead_code::DeadCodeElimination;
