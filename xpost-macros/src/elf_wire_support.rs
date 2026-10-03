//! Wire helpers expanded into the invoking module (`image::elf`).

use proc_macro::TokenStream;
use quote::quote;

/// Emits `ElfWireError`, `FlashBuffer`, and `visit_table_bytes` into the caller module.
pub fn expand_support(_input: TokenStream) -> TokenStream {
    quote! {
        /// Construction / parse errors for Compact ELF wire reads.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub(crate) enum ElfWireError {
            AddressOverflow,
            OutOfBounds,
            Storage,
            Truncated,
            MetadataTooLarge,
        }

        /// Fixed-capacity bulk read buffer (`heapless::Vec`).
        pub(crate) struct FlashBuffer<const N: usize> {
            bytes: ::heapless::Vec<u8, N>,
        }

        impl<const N: usize> FlashBuffer<N> {
            #[inline]
            pub(crate) fn read(
                table: &mut dyn crate::xdp::XdpTableStorage,
                offset: usize,
                len: usize,
            ) -> Result<Self, ElfWireError> {
                if len > N {
                    return Err(ElfWireError::OutOfBounds);
                }
                let end = offset
                    .checked_add(len)
                    .ok_or(ElfWireError::AddressOverflow)?;
                if end > table.capacity() {
                    return Err(ElfWireError::OutOfBounds);
                }
                let mut bytes = ::heapless::Vec::<u8, N>::new();
                bytes
                    .resize_default(len)
                    .map_err(|_| ElfWireError::OutOfBounds)?;
                table
                    .read(offset, bytes.as_mut())
                    .map_err(|_| ElfWireError::Storage)?;
                Ok(Self { bytes })
            }

            #[inline(always)]
            pub(crate) fn as_slice(&self) -> &[u8] {
                self.bytes.as_slice()
            }

            #[inline(always)]
            pub(crate) fn into_inner(self) -> ::heapless::Vec<u8, N> {
                self.bytes
            }
        }

        /// Streaming read for image SHA only (not metadata getters).
        #[inline]
        pub(crate) fn visit_table_bytes(
            table: &mut dyn crate::xdp::XdpTableStorage,
            base: usize,
            len: usize,
            mut visit: impl FnMut(&[u8]),
        ) -> Result<(), ElfWireError> {
            #[repr(C, align(4))]
            struct Block([u8; 68]);

            #[inline(always)]
            const fn align_down4(x: usize) -> usize {
                x & !3
            }
            #[inline(always)]
            const fn align_up4(x: usize) -> usize {
                (x + 3) & !3
            }

            if len == 0 {
                return Ok(());
            }
            let end = base
                .checked_add(len)
                .ok_or(ElfWireError::AddressOverflow)?;
            if align_up4(end) > table.capacity() {
                return Err(ElfWireError::OutOfBounds);
            }

            let mut block = Block([0; 68]);
            let mut current = base;
            while current < end {
                let n = (end - current).min(64);
                let prefix = current & 3;
                let physical_start = align_down4(current);
                let physical_len = align_up4(prefix + n);
                if physical_start.saturating_add(physical_len) > table.capacity() {
                    return Err(ElfWireError::OutOfBounds);
                }
                table
                    .read(physical_start, &mut block.0[..physical_len])
                    .map_err(|_| ElfWireError::Storage)?;
                visit(&block.0[prefix..prefix + n]);
                current += n;
            }
            Ok(())
        }
    }
    .into()
}
