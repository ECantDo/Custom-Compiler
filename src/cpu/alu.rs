use crate::cpu::{Long, UnsignedWord, Word};

#[derive(Copy, Clone)]
struct CpuFlags {
	carry_flag: bool,
	is_zero: bool,
}

/*
    Opcode: 8 bits:
    0bUUUIMMMM
    U -> Upper code; specific instruction for the ALU
    I -> Immediate flag; is this instruction using an immediate value?
    M -> Main code; what part of the CPU is being used ... i.e. main ALU vs barrel shifter
*/
const MAIN_OP_MASK: u8 = 0x0F; // first 4 bits of the opcode
const IS_IMM_MASK: u8 = 0x10; // the fifth bit
const UPPER_OP_MASK: u8 = 0xE0; // last 3 bits of the opcode

const MAIN_OP_ALU: u8 = 0x01;
const MAIN_OP_SHIFT: u8 = 0x02;
const MAIN_OP_ADDF: u8 = 0x03;

fn run_alu_operation(opcode: u8, input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
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

const WORD_MASK: Long = UnsignedWord::MAX as Long; // Should be 0x0000FFFF

trait AluOperation {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word;
}

struct AluAdd;
impl AluOperation for AluAdd {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
		let mut result: Long = input_a as Long + input_b as Long;
		flags.carry_flag = (result & !WORD_MASK) != 0;
		result &= WORD_MASK;
		flags.is_zero = result == 0;

		result as Word
	}
}


struct AluSub;
impl AluOperation for AluSub {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
		let mut result: Long = (input_a as Long).wrapping_sub(input_b as Long);
		flags.carry_flag = (result & !WORD_MASK) != 0; // borrow, if you're treating carry_flag as "borrow out" for SUB
		result &= WORD_MASK;
		flags.is_zero = result == 0;
		result as Word
	}
}

struct AluXor;
impl AluOperation for AluXor {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
		let result = input_a ^ input_b;
		flags.is_zero = result == 0;
		flags.carry_flag = false;
		result
	}
}

struct AluOr;
impl AluOperation for AluOr {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
		let result = input_a | input_b;
		flags.is_zero = result == 0;
		flags.carry_flag = false;
		result
	}
}

struct AluAnd;
impl AluOperation for AluAnd {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
		let result = input_a & input_b;
		flags.is_zero = result == 0;
		flags.carry_flag = false;
		result
	}
}

struct AluXnor;
impl AluOperation for AluXnor {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
		let result = !(input_a ^ input_b);
		flags.is_zero = result == 0;
		flags.carry_flag = false;
		result
	}
}

struct AluNor;
impl AluOperation for AluNor {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
		let result = !(input_a | input_b);
		flags.is_zero = result == 0;
		flags.carry_flag = false;
		result
	}
}

struct AluNand;
impl AluOperation for AluNand {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
		let result = !(input_a & input_b);
		flags.is_zero = result == 0;
		flags.carry_flag = false;
		result
	}
}

struct AluShiftLeftLogical;
impl AluOperation for AluShiftLeftLogical {
	fn operation(input_a: Word, input_b: Word, _flags: &mut CpuFlags) -> Word {
		let shift = (input_b & 0xF) as Long;
		let result = ((input_a as Long) << shift) & WORD_MASK;
		result as Word
	}
}

struct AluShiftRightArithmetic;
impl AluOperation for AluShiftRightArithmetic {
	fn operation(input_a: Word, input_b: Word, _flags: &mut CpuFlags) -> Word {
		let shift = (input_b & 0xF) as Long;
		// Sign-extend input_a to a signed type before shifting so the sign bit propagates
		let result = ((input_a as i16 as Long) >> shift) & WORD_MASK;
		result as Word
	}
}

struct AluShiftRightLogical;
impl AluOperation for AluShiftRightLogical {
	fn operation(input_a: Word, input_b: Word, _flags: &mut CpuFlags) -> Word {
		let shift = (input_b & 0xF) as Long;
		let result = ((input_a as Long) >> shift) & WORD_MASK;
		result as Word
	}
}

struct AluAddWithFlags;
impl AluOperation for AluAddWithFlags {
	fn operation(input_a: Word, input_b: Word, flags: &mut CpuFlags) -> Word {
		let cin: Long = if flags.carry_flag { 1 } else { 0 };
		let mut result: Long = input_a as Long + input_b as Long + cin;
		flags.carry_flag = (result & !WORD_MASK) != 0;
		result &= WORD_MASK;
		flags.is_zero = result == 0;
		result as Word
	}
}
// =================================================================================================
// Testing
// =================================================================================================

#[test]
fn test() {
	debug_assert!(WORD_MASK == 0xffff);
}
