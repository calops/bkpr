//! Parser-boundary ABI introduced by Ghostty 93967b7. Mirrors terminal.h.
pub mod TerminalBoundary {
    pub type Type = ::std::os::raw::c_int;
    pub const GROUND: Type = 1;
    pub const APC_START: Type = 1 << 1;
    pub const APC_END: Type = 1 << 2;
    pub const DCS_START: Type = 1 << 3;
    pub const DCS_END: Type = 1 << 4;
    pub const OSC_START: Type = 1 << 5;
    pub const OSC_END: Type = 1 << 6;
}
pub mod TerminalStringKind {
    pub type Type = ::std::os::raw::c_int;
    pub const APC: Type = 0;
    pub const DCS: Type = 1;
    pub const OSC: Type = 2;
}
pub mod TerminalStringIntroducer {
    pub type Type = ::std::os::raw::c_int;
    pub const ESC: Type = 0;
    pub const C1: Type = 1;
}
pub mod TerminalStringEnd {
    pub type Type = ::std::os::raw::c_int;
    pub const ST_ESC: Type = 0;
    pub const ST_C1: Type = 1;
    pub const CANCELLED: Type = 2;
    pub const ABORTED: Type = 3;
    pub const BEL: Type = 4;
}
#[repr(C)]
#[derive(Debug, Default, Copy, Clone)]
pub struct TerminalWriteStop {
    pub size: usize,
    pub consumed: usize,
    pub reached: u32,
    pub kind: TerminalStringKind::Type,
    pub introducer: TerminalStringIntroducer::Type,
    pub end: TerminalStringEnd::Type,
    pub byte: u8,
}
