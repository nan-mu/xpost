//! App-mode transport owned by [`crate::client::SerialHandler`].

use std::io;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

#[cfg(all(test, feature = "dev"))]
use super::byte_port::AppBytePort;
#[cfg(all(test, feature = "dev"))]
use super::memory::MemoryClient;
use super::uart::{Interrupt, Uart};
use super::Port;
use crate::client::record::SharedRecord;

/// Injectable App UART transport (USB production; memory only in `dev` tests).
pub(crate) enum AppLink {
    Usb(Arc<Port>),
    #[cfg(all(test, feature = "dev"))]
    Memory(MemoryClient),
}

/// Opened App UART; USB sessions hold the port gate until drop.
pub(crate) enum OpenedUart<'a> {
    Usb(super::UartSession<'a>),
    #[cfg(all(test, feature = "dev"))]
    Memory(Uart),
}

impl Deref for OpenedUart<'_> {
    type Target = Uart;

    fn deref(&self) -> &Uart {
        match self {
            Self::Usb(s) => s,
            #[cfg(all(test, feature = "dev"))]
            Self::Memory(u) => u,
        }
    }
}

impl DerefMut for OpenedUart<'_> {
    fn deref_mut(&mut self) -> &mut Uart {
        match self {
            Self::Usb(s) => s,
            #[cfg(all(test, feature = "dev"))]
            Self::Memory(u) => u,
        }
    }
}

impl AppLink {
    pub fn interrupt(&self) -> &Interrupt {
        match self {
            Self::Usb(port) => port.interrupt(),
            #[cfg(all(test, feature = "dev"))]
            Self::Memory(mem) => mem.interrupt(),
        }
    }

    pub fn open_uart(&self, record: SharedRecord) -> io::Result<OpenedUart<'_>> {
        match self {
            Self::Usb(port) => Ok(OpenedUart::Usb(port.open_uart(record)?)),
            #[cfg(all(test, feature = "dev"))]
            Self::Memory(mem) => {
                mem.interrupt().clear();
                let uart = Uart::open_port(mem.io().try_clone_box()?, record)?;
                Ok(OpenedUart::Memory(uart))
            }
        }
    }

    pub fn port(&self) -> Option<&Port> {
        match self {
            Self::Usb(port) => Some(port),
            #[cfg(all(test, feature = "dev"))]
            Self::Memory(_) => None,
        }
    }
}
