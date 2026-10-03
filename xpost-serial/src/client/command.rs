//! Host-only impls for [`crate::r#type::ops`] (Display / Debug / helpers).

use core::fmt;

use crate::r#type::command as wire;
use crate::r#type::ops::{
    Clear, Command, Disable, Enable, Flash, FlashOp, Get, Update,
};

impl Command {
    /// Convert App variants to wire [`wire::Command`]. `Flash` → `None`.
    pub fn to_wire(&self) -> Option<wire::Command> {
        match self {
            Self::Ping => Some(wire::Command::Ping),
            Self::Enable(Enable::Xdp) => Some(wire::Command::EnableXdp),
            Self::Disable(Disable::Xdp) => Some(wire::Command::DisableXdp),
            Self::Clear(Clear::Map { name }) => Some(wire::Command::ClearMap {
                name: name.clone(),
            }),
            Self::Clear(Clear::Perf) => Some(wire::Command::ClearPerf),
            Self::Get(Get::Map { name }) => Some(wire::Command::GetMap {
                name: name.clone(),
            }),
            Self::Get(Get::Perf) => Some(wire::Command::GetPerf),
            Self::Update(u) => Some(wire::Command::UpdateMap {
                name: u.name.clone(),
                key: u.key.clone(),
                value: u.value.clone(),
            }),
            Self::Flash(_) => None,
        }
    }

    pub fn is_flash(&self) -> bool {
        matches!(self, Self::Flash(_))
    }

    pub fn is_get_map(&self) -> bool {
        matches!(self, Self::Get(Get::Map { .. }))
    }

    pub fn is_get_perf(&self) -> bool {
        matches!(self, Self::Get(Get::Perf))
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Flash(flash) => flash.fmt(f),
            other => match other.to_wire() {
                Some(w) => w.fmt(f),
                None => Ok(()),
            },
        }
    }
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ping => write!(f, "Ping"),
            Self::Enable(v) => f.debug_tuple("Enable").field(v).finish(),
            Self::Disable(v) => f.debug_tuple("Disable").field(v).finish(),
            Self::Clear(v) => f.debug_tuple("Clear").field(v).finish(),
            Self::Get(v) => f.debug_tuple("Get").field(v).finish(),
            Self::Update(v) => f.debug_tuple("Update").field(v).finish(),
            Self::Flash(v) => f.debug_tuple("Flash").field(v).finish(),
        }
    }
}

impl fmt::Display for Flash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Begin(op) => write!(f, "FLASH begin {}", op.label()),
            Self::End(op) => write!(f, "FLASH end {}", op.label()),
            Self::Failed { op, msg } => write!(f, "FLASH failed {} {msg}", op.label()),
        }
    }
}

impl fmt::Debug for Flash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Begin(op) => f.debug_tuple("Begin").field(op).finish(),
            Self::End(op) => f.debug_tuple("End").field(op).finish(),
            Self::Failed { op, msg } => f
                .debug_struct("Failed")
                .field("op", op)
                .field("msg", msg)
                .finish(),
        }
    }
}

impl FlashOp {
    pub fn label(&self) -> String {
        match self {
            Self::Fw { .. } => "fw".into(),
            Self::Xdp { name, .. } => format!("xdp name={name}"),
            Self::Clear { names } if names.is_empty() => "clear xdp *".into(),
            Self::Clear { names } => format!("clear xdp {}", names.join(",")),
            Self::List => "get xdp".into(),
        }
    }
}

impl fmt::Debug for FlashOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fw {
                firmware,
                partitions,
            } => f
                .debug_struct("Fw")
                .field("firmware", firmware)
                .field("partitions", partitions)
                .finish(),
            Self::Xdp { name, path } => f
                .debug_struct("Xdp")
                .field("name", name)
                .field("path", path)
                .finish(),
            Self::Clear { names } => f.debug_struct("Clear").field("names", names).finish(),
            Self::List => write!(f, "List"),
        }
    }
}

impl fmt::Debug for Enable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Xdp => write!(f, "Xdp"),
        }
    }
}

impl fmt::Debug for Disable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Xdp => write!(f, "Xdp"),
        }
    }
}

impl fmt::Debug for Clear {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Map { name } => f.debug_struct("Map").field("name", name).finish(),
            Self::Perf => write!(f, "Perf"),
        }
    }
}

impl fmt::Debug for Get {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Map { name } => f.debug_struct("Map").field("name", name).finish(),
            Self::Perf => write!(f, "Perf"),
        }
    }
}

impl fmt::Debug for Update {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Update")
            .field("name", &self.name)
            .field("key", &self.key)
            .field("value", &self.value)
            .finish()
    }
}

impl fmt::Debug for crate::r#type::ops::Chip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Esp32c3 => write!(f, "Esp32c3"),
        }
    }
}

impl fmt::Display for crate::r#type::ops::Chip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Esp32c3 => write!(f, "esp32c3"),
        }
    }
}
