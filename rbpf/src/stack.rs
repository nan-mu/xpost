// SPDX-License-Identifier: (Apache-2.0 OR MIT)

#[cfg(not(feature = "mcu"))]
use core::any::Any;

#[cfg_attr(feature = "mcu", allow(unused_imports))]
use crate::{
    ebpf::{self, LOCAL_FUNCTION_STACK_SIZE},
    lib::*,
    StackUsageCalculator,
};

#[derive(Debug, Copy, Clone)]
pub struct StackFrame {
    return_address: usize,
    saved_registers: [u64; 4],
    stack_usage: StackUsageType,
}

impl StackFrame {
    /// Create a new stack frame
    pub const fn new() -> Self {
        Self {
            return_address: 0,
            saved_registers: [0; 4],
            stack_usage: StackUsageType::Default,
        }
    }
    /// Save the callee-saved registers
    pub fn save_registers(&mut self, regs: &[u64]) {
        self.saved_registers.copy_from_slice(regs);
    }

    /// Get the callee-saved registers
    pub fn get_registers(&self) -> [u64; 4] {
        self.saved_registers
    }

    /// Save the return address
    pub fn save_return_address(&mut self, address: usize) {
        self.return_address = address;
    }

    /// Get the return address
    pub fn get_return_address(&self) -> usize {
        self.return_address
    }

    /// Set the stack usage
    pub fn set_stack_usage(&mut self, usage: StackUsageType) {
        self.stack_usage = usage;
    }
    /// Get the stack usage
    pub fn get_stack_usage(&self) -> StackUsageType {
        self.stack_usage
    }
}

#[derive(Debug, Copy, Clone)]
pub enum StackUsageType {
    Default,
    Custom(u16),
}

impl StackUsageType {
    pub fn stack_usage(&self) -> u16 {
        match self {
            StackUsageType::Default => LOCAL_FUNCTION_STACK_SIZE,
            StackUsageType::Custom(size) => *size,
        }
    }
}

pub struct StackVerifier {
    #[cfg_attr(feature = "mcu", allow(dead_code))]
    calculator: Option<StackUsageCalculator>,
    /// Local fork: absent under `mcu`, which never carries calculator state.
    #[cfg(not(feature = "mcu"))]
    data: Option<Box<dyn Any>>,
}

impl StackVerifier {
    pub fn new(
        stack_usage_calculator: Option<StackUsageCalculator>,
        #[cfg(not(feature = "mcu"))] data: Option<Box<dyn Any>>,
    ) -> Self {
        Self {
            calculator: stack_usage_calculator,
            #[cfg(not(feature = "mcu"))]
            data,
        }
    }
    /// Validate the stack usage of a program.
    ///
    /// Local fork: absent under `mcu`. It fills a per-function `HashMap`, and
    /// the MCU build uses `StackUsage::uniform` instead.
    #[cfg(not(feature = "mcu"))]
    pub fn stack_validate(&mut self, prog: &[u8]) -> Result<StackUsage, Error> {
        let mut stack_usage = HashMap::new();
        let ty = self.calculate_stack_usage_for_local_func(prog, 0)?;
        stack_usage.insert(0, ty);
        for idx in 0..prog.len() / ebpf::INSN_SIZE {
            let insn = ebpf::get_insn(prog, idx);
            if insn.opc == ebpf::CALL {
                let dst_insn_ptr = idx as isize + 1 + insn.imm as isize;
                let ty = self.calculate_stack_usage_for_local_func(prog, dst_insn_ptr as usize)?;
                stack_usage.insert(dst_insn_ptr as usize, ty);
            }
        }
        Ok(StackUsage(StackUsageMap::PerFunction(stack_usage)))
    }

    /// Calculate the stack usage for a local function
    #[cfg(not(feature = "mcu"))]
    fn calculate_stack_usage_for_local_func(
        &mut self,
        prog: &[u8],
        pc: usize,
    ) -> Result<StackUsageType, Error> {
        let mut ty = StackUsageType::Default;
        match self.calculator {
            Some(calculator) => {
                ty = StackUsageType::Custom(calculator(prog, pc, self.data.as_mut().unwrap()));
            }
            None => return Ok(ty),
        }
        Ok(ty)
    }
}

pub struct StackUsage(StackUsageMap);

/// Where per-function stack sizes come from.
///
/// Upstream keeps a `HashMap` filled by a caller-provided calculator. That
/// allocates once per program load, which is fine on a host and wrong on an
/// MCU. The `mcu` build can use [`StackUsage::uniform`] instead: every frame
/// gets the same size, so the map disappears and the lookup becomes an `Option`
/// that is always `Some` when a program calls local functions.
enum StackUsageMap {
    #[cfg(not(feature = "mcu"))]
    PerFunction(HashMap<usize, StackUsageType>),
    #[cfg(feature = "mcu")]
    Uniform(StackUsageType),
}

impl StackUsage {
    /// One stack size for every frame, including the entry function.
    ///
    /// Allocation-free. A program that never issues a local `call` ignores this
    /// value entirely, because `r10` only moves on such a call.
    #[cfg(feature = "mcu")]
    pub const fn uniform(usage: StackUsageType) -> Self {
        Self(StackUsageMap::Uniform(usage))
    }

    /// Get the stack usage for a local function
    pub fn stack_usage_for_local_func(&self, pc: usize) -> Option<StackUsageType> {
        match &self.0 {
            #[cfg(not(feature = "mcu"))]
            StackUsageMap::PerFunction(map) => map.get(&pc).cloned(),
            #[cfg(feature = "mcu")]
            StackUsageMap::Uniform(usage) => {
                let _ = pc;
                Some(*usage)
            }
        }
    }
}
