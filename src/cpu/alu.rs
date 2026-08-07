use crate::cpu::{UnsignedWord, Word};

#[derive(Copy, Clone)]
pub struct CpuFlags {
	pub carry_flag: bool,
	pub is_zero: bool,
	pub overflow_flag: bool, // signed overflow; used with sign_flag to resolve signed vs unsigned branches
	pub sign_flag: bool, // top bit of the result; alone it's not enough for signed compares (see overflow_flag)
}

impl Default for CpuFlags {
	fn default() -> Self {
		Self { carry_flag: false, is_zero: false, overflow_flag: false, sign_flag: false }
	}
}

/*
 * Opcode: 8 bits:
 * 0bUUUIMMMM
 * U -> Upper code; specific instruction for the ALU
 * I -> Immediate flag; is this instruction using an immediate value?
 * M -> Main code; what part of the CPU is being used ... i.e. main ALU vs barrel shifter
*/
pub const MAIN_OP_MASK: u8 = 0x0F; // first 4 bits of the opcode
pub const IS_IMM_MASK: u8 = 0x10; // the fifth bit
pub const UPPER_OP_MASK: u8 = 0xE0; // last 3 bits of the opcode

const MAIN_OP_ALU: u8 = 0x01;
const MAIN_OP_SHIFT: u8 = 0x02;
const MAIN_OP_ADDF: u8 = 0x03;

pub fn run_alu_operation(opcode: u8, input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
	match opcode & MAIN_OP_MASK {
		MAIN_OP_ALU => match (opcode & UPPER_OP_MASK) >> 5 {
			0 => AluAdd::operation(input_a, input_b, flags),
			1 => AluSub::operation(input_a, input_b, flags),
			2 => AluXor::operation(input_a, input_b, flags),
			3 => AluOr::operation(input_a, input_b, flags),
			4 => AluAnd::operation(input_a, input_b, flags),
			5 => AluXnor::operation(input_a, input_b, flags),
			6 => AluNor::operation(input_a, input_b, flags),
			7 => AluNand::operation(input_a, input_b, flags),
			_ => 0,
		},
		MAIN_OP_SHIFT => match (opcode & UPPER_OP_MASK) >> 5 {
			0 => AluShiftLeftLogical::operation(input_a, input_b, flags),
			1 => AluShiftRightArithmetic::operation(input_a, input_b, flags),
			2 => AluShiftRightLogical::operation(input_a, input_b, flags),
			_ => 0,
		},
		MAIN_OP_ADDF => AluAddWithFlags::operation(input_a, input_b, flags),
		_ => 0,
	}
}

// =================================================================================================
// ALU Operations
// =================================================================================================

trait AluOperation {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word;
}

struct AluAdd;
impl AluOperation for AluAdd {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
		let (result, overflowed) = input_a.overflowing_add(input_b);
		flags.overflow_flag = overflowed;

		let (_, carried) = (input_a as UnsignedWord).overflowing_add(input_b as UnsignedWord);
		flags.carry_flag = carried;

		flags.is_zero = result == 0;
		flags.sign_flag = result < 0;
		result
	}
}


struct AluSub;
impl AluOperation for AluSub {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
		let (result, overflowed) = input_a.overflowing_sub(input_b);
		flags.overflow_flag = overflowed;

		let (_, borrowed) = (input_a as UnsignedWord).overflowing_sub(input_b as UnsignedWord);
		flags.carry_flag = borrowed; // borrow-out, if you're treating carry_flag as "borrow" for SUB

		flags.is_zero = result == 0;
		flags.sign_flag = result < 0;
		result
	}
}

struct AluXor;
impl AluOperation for AluXor {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
		let result = input_a ^ input_b;
		flags.is_zero = result == 0;
		flags.carry_flag = false;
		flags.overflow_flag = false;
		flags.sign_flag = result < 0;
		result
	}
}

struct AluOr;
impl AluOperation for AluOr {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
		let result = input_a | input_b;
		flags.is_zero = result == 0;
		flags.carry_flag = false;
		flags.overflow_flag = false;
		flags.sign_flag = result < 0;
		result
	}
}

struct AluAnd;
impl AluOperation for AluAnd {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
		let result = input_a & input_b;
		flags.is_zero = result == 0;
		flags.carry_flag = false;
		flags.overflow_flag = false;
		flags.sign_flag = result < 0;
		result
	}
}

struct AluXnor;
impl AluOperation for AluXnor {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
		let result = !(input_a ^ input_b);
		flags.is_zero = result == 0;
		flags.carry_flag = false;
		flags.overflow_flag = false;
		flags.sign_flag = result < 0;
		result
	}
}

struct AluNor;
impl AluOperation for AluNor {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
		let result = !(input_a | input_b);
		flags.is_zero = result == 0;
		flags.carry_flag = false;
		flags.overflow_flag = false;
		flags.sign_flag = result < 0;
		result
	}
}

struct AluNand;
impl AluOperation for AluNand {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
		let result = !(input_a & input_b);
		flags.is_zero = result == 0;
		flags.carry_flag = false;
		flags.overflow_flag = false;
		flags.sign_flag = result < 0;
		result
	}
}

struct AluShiftLeftLogical;
impl AluOperation for AluShiftLeftLogical {
	fn operation(input_a: Word, input_b: Word, _flags: &mut CpuFlags) -> Word {
		let shift = input_b & 0xF;
		input_a << shift
	}
}

struct AluShiftRightArithmetic;
impl AluOperation for AluShiftRightArithmetic {
	fn operation(input_a: Word, input_b: Word, _flags: &mut CpuFlags) -> Word {
		let shift = input_b & 0xF;
		// Word is signed (i16), so >> is already arithmetic (sign-extending)
		input_a >> shift
	}
}

struct AluShiftRightLogical;
impl AluOperation for AluShiftRightLogical {
	fn operation(input_a: Word, input_b: Word, _flags: &mut CpuFlags) -> Word {
		let shift = (input_b & 0xF) as UnsignedWord;
		// Reinterpret as UnsignedWord so >> zero-fills (logical) instead of sign-extending
		((input_a as UnsignedWord) >> shift) as Word
	}
}

struct AluAddWithFlags;
impl AluOperation for AluAddWithFlags {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
		let cin_unsigned: UnsignedWord = if flags.carry_flag { 1 } else { 0 };
		let cin_signed: Word = cin_unsigned as Word;

		// Chain two overflowing_adds since a 3-operand add has no single builtin;
		// carry-out is true if either step carried.
		let (partial_u, carry1) = (input_a as UnsignedWord).overflowing_add(input_b as UnsignedWord);
		let (_result_u, carry2) = partial_u.overflowing_add(cin_unsigned);
		flags.carry_flag = carry1 || carry2;

		let (partial_s, overflow1) = input_a.overflowing_add(input_b);
		let (result, overflow2) = partial_s.overflowing_add(cin_signed);
		flags.overflow_flag = overflow1 || overflow2;

		flags.is_zero = result == 0;
		flags.sign_flag = result < 0;
		result
	}
}