# XPOST

XPOST loads and runs XDP programs on microcontrollers.

## Language

**Firmware ELF**:
The linked board application that provides the target memory layout.

**XDP ELF**:
The container for an XDP image and its metadata.
_Avoid_: Compact ELF

**SRAM slot**:
A reserved SRAM region for a loaded XDP image.

**Cold storage cell**:
A fixed-size region of the XDP flash partition for one XDP ELF.
