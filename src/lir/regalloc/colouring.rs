use crate::lir::target::Target;

/// Where a [virtual register](crate::lir::VReg) lives after allocation
pub enum Location<T: Target> {
    Reg(T::Reg),

    /// Offset from %rbp
    Stack(i32),
}

// pub fn colour<T: Target>
