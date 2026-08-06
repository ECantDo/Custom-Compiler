use std::fmt;

/// Ports `Assembler.Exceptions.OpcodeExistsException`, plus the assorted
/// `IllegalArgumentException` / `UnsupportedOperationException` throw sites
/// scattered through the original `Build.java`. Java threw a grab-bag of
/// unchecked exceptions from deep inside the call stack and let them
/// propagate to `main`; Rust doesn't have unchecked exceptions, so this
/// enum + `Result` plays that same role explicitly.
#[derive(Debug)]
pub enum AssemblerError {
    /// Java: `OpcodeExistsException` - despite the name, this was thrown
    /// when a mnemonic did NOT exist (`Opcodes.generateOperation` returned
    /// null). Kept the odd naming/semantics as-is for fidelity.
    UnknownOpcode(String),
    InvalidArgument(String),
    InvalidPseudoOpcode(String),
    UnsupportedEncoding(String),
    Io(std::io::Error),
}

impl fmt::Display for AssemblerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AssemblerError::UnknownOpcode(msg) => write!(f, "{msg}"),
            AssemblerError::InvalidArgument(msg) => write!(f, "{msg}"),
            AssemblerError::InvalidPseudoOpcode(msg) => write!(f, "{msg}"),
            AssemblerError::UnsupportedEncoding(msg) => write!(f, "{msg}"),
            AssemblerError::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for AssemblerError {}

impl From<std::io::Error> for AssemblerError {
    fn from(err: std::io::Error) -> Self {
        AssemblerError::Io(err)
    }
}
