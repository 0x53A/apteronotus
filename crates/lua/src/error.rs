use piccolo::ExternError;
use std::{error::Error, fmt};

#[derive(Debug)]
pub enum EvalError {
    Lua(ExternError),
    SourceLimit { used: usize, limit: usize },
    FuelLimit { limit: u32 },
    MemoryLimit { used: usize, limit: usize },
    Binding(String),
}

impl fmt::Display for EvalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvalError::Lua(error) => error.fmt(f),
            EvalError::SourceLimit { used, limit } => {
                write!(f, "source uses {used} bytes, limit is {limit}")
            }
            EvalError::FuelLimit { limit } => {
                write!(f, "Lua evaluation exceeded its fuel limit of {limit}")
            }
            EvalError::MemoryLimit { used, limit } => {
                write!(f, "Lua evaluation uses {used} bytes, limit is {limit}")
            }
            EvalError::Binding(message) => f.write_str(message),
        }
    }
}

impl Error for EvalError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            EvalError::Lua(error) => Some(error),
            _ => None,
        }
    }
}

impl From<ExternError> for EvalError {
    fn from(error: ExternError) -> Self {
        Self::Lua(error)
    }
}
