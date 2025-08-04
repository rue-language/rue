//! Portable Intermediate Representation (PIR)
//!
//! PIR is a platform-independent instruction set that serves as the target
//! for code generation from MIR and the source for lowering to machine code.

use crate::types::RueType;

/// Virtual register - will be allocated to a physical register or stack slot
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VReg(pub u32);

/// Abstract physical register identifier - maps to target-specific registers during lowering
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PhysicalRegId(pub u8);

/// Value operand for instructions
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    VReg(VReg),
    SignedImm(i64),
    UnsignedImm(u64),
    PhysicalReg(PhysicalRegId),
}

/// Binary operations
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

/// Label space to distinguish between runtime and user labels
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LabelSpace {
    Runtime,
    User,
}

/// Type-safe label that tracks which space it belongs to
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Label {
    id: u32,
    space: LabelSpace,
}

impl Label {
    /// Create a new runtime label
    pub fn runtime(id: u32) -> Self {
        Label {
            id,
            space: LabelSpace::Runtime,
        }
    }

    /// Create a new user label
    pub fn user(id: u32) -> Self {
        Label {
            id,
            space: LabelSpace::User,
        }
    }

    /// Get the raw ID without offset
    pub fn id(&self) -> u32 {
        self.id
    }

    /// Get the label space
    pub fn space(&self) -> LabelSpace {
        self.space
    }

    /// Convert to a machine label ID with proper offset
    /// Runtime labels keep their IDs, user labels are offset by runtime_label_count
    pub fn to_machine_id(&self, runtime_label_count: u32) -> u32 {
        match self.space {
            LabelSpace::Runtime => self.id,
            LabelSpace::User => self.id + runtime_label_count,
        }
    }

    /// Create a label from a machine ID, determining its space based on offset
    pub fn from_machine_id(machine_id: u32, runtime_label_count: u32) -> Self {
        // Use <= to correctly handle the case when runtime_label_count == 0
        if runtime_label_count == 0 || machine_id >= runtime_label_count {
            Label::user(machine_id - runtime_label_count)
        } else {
            Label::runtime(machine_id)
        }
    }
}

/// Portable Intermediate Representation - Platform-independent instruction set
///
/// Examples:
/// - `2 + 3` generates: Copy{v0, Imm(2)}, Copy{v1, Imm(3)}, BinaryOp{v2, v0, v1, Add}
/// - `x = 42` generates: Copy{v0, Imm(42)}, then maps variable "x" to v0
/// - `n * factorial(n-1)` generates: Push{v0}, Call{v1, "factorial", [v2]}, Pop{v3}, BinaryOp{v4, v3, v1, Mul}
#[derive(Debug, Clone)]
pub enum PIR {
    // Data movement
    Copy {
        dest: VReg,
        src: Value,
    },
    // Block parameter assignment - for MIR block parameters (never reads dest's old value)
    BlockParamAssign {
        dest: VReg,
        src: Value,
    },

    // Arithmetic and comparison operations
    BinaryOp {
        dest: VReg,
        lhs: Value,
        rhs: Value,
        op: BinOp,
    },
    // Type-aware binary operations (for proper i32 overflow handling)
    TypedBinaryOp {
        dest: VReg,
        lhs: Value,
        rhs: Value,
        op: BinOp,
        ty: RueType,
    },

    // Memory operations
    Load {
        dest: VReg,
        offset: i64,
    }, // Load from stack
    Store {
        src: VReg,
        offset: i64,
    }, // Store to stack

    // Stack operations for value preservation
    Push {
        src: Value,
    }, // Push register to stack
    Pop {
        dest: VReg,
    }, // Pop from stack to register

    // Control flow
    Label(Label),
    Jump(Label),
    Branch {
        condition: VReg,
        true_label: Label,
        false_label: Label,
    },

    // Function operations
    Call {
        dest: Option<VReg>,
        function: String,
        args: Vec<VReg>,
    },
    Return {
        value: Option<VReg>,
    },

    // System operations
    Syscall {
        result: VReg,
        syscall_num: VReg,
        args: Vec<VReg>,
    },

    // Register preservation for calling convention
    SaveRegisters {
        registers: Vec<PhysicalRegId>,
    },
    RestoreRegisters {
        registers: Vec<PhysicalRegId>,
    },

    // Stack frame management
    EnterFrame,
    LeaveFrame,

    // Aggregate operations
    AllocateAggregate {
        dest: VReg,     // Result register containing pointer to allocated memory
        size: i64,      // Size of aggregate in bytes
        alignment: i64, // Alignment requirement (typically 8)
    },
    CopyAggregate {
        dest: VReg, // Destination pointer
        src: VReg,  // Source pointer
        size: i64,  // Size in bytes
    },
    ZeroAggregate {
        dest: VReg, // Destination pointer
        size: i64,  // Size in bytes
    },
    LoadField {
        dest: VReg,          // Destination register
        base: VReg,          // Base pointer register
        offset: i64,         // Field offset in bytes
        field_type: RueType, // Type of field being loaded
    },
    StoreField {
        base: VReg,          // Base pointer register
        offset: i64,         // Field offset in bytes
        src: Value,          // Source value to store
        field_type: RueType, // Type of field being stored
    },

    // Dynamic array operations with bounds checking
    /// Check array bounds and trap if index is out of bounds
    ArrayBoundsCheck {
        array_base: VReg,  // Base pointer to array
        index: VReg,       // Index to check
        array_len: u64,    // Array length (compile-time constant)
        trap_label: Label, // Label to jump to on bounds violation
    },
    /// Load from array at dynamic index (assumes bounds check already done)
    DynamicLoadField {
        dest: VReg,            // Destination register
        base: VReg,            // Base pointer register
        index: VReg,           // Index register (must be bounds-checked)
        element_size: i64,     // Size of each element in bytes
        element_type: RueType, // Type of element being loaded
    },
    /// Store to array at dynamic index (assumes bounds check already done)
    DynamicStoreField {
        base: VReg,            // Base pointer register
        index: VReg,           // Index register (must be bounds-checked)
        src: Value,            // Source value to store
        element_size: i64,     // Size of each element in bytes
        element_type: RueType, // Type of element being stored
    },

    // Runtime error handling
    /// Trap with error message (for bounds violations, etc.)
    Trap {
        message: String, // Error message
    },

    // Optimized inline aggregate operations for small sizes
    InlineCopyAggregate {
        dest: VReg, // Destination pointer
        src: VReg,  // Source pointer
        size: i64,  // Size in bytes (≤16 for inline optimization)
    },
    InlineZeroAggregate {
        dest: VReg, // Destination pointer
        size: i64,  // Size in bytes (≤8 for inline optimization)
    },
}
