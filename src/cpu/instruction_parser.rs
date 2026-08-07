use crate::assembler::operations::opcodes::OPCODE_MAP;
use crate::cpu::alu::{IS_IMM_MASK, MAIN_OP_MASK, UPPER_OP_MASK};
use crate::cpu::{Instruction, UnsignedWord};

pub struct InstructionInfo {
	immediate: UnsignedWord,
	opcode: u8,
	write_back_reg: u8,
	reg1: u8,
	reg2: u8,
}

impl Default for InstructionInfo {
	fn default() -> Self {
		Self {
			immediate: 0,
			opcode: 0,
			write_back_reg: 0,
			reg1: 0,
			reg2: 0,
		}
	}
}

//@formatter:off
// General stuff
const RD_REG_MASK: Instruction =	0x00001F00;
const RD_REG_SHIFT: Instruction = 8;
const RS1_REG_MASK: Instruction = 	0x0003E000;
const RS1_REG_SHIFT: Instruction = 13;
const RS2_REG_MASK: Instruction = 	0x007C0000;
const RS2_REG_SHIFT: Instruction = 18;

const STAND_IMM_MASK: Instruction = 0xFFFC0000;
const STAND_IMM_SHIFT: Instruction = 18;

// Branching stuff
const LOWER_IMM_BRANCH_MASK: Instruction = RD_REG_MASK;
const LOWER_IMM_BRANCH_SHIFT: Instruction = RD_REG_SHIFT;
const UPPER_IMM_BRANCH_MASK: Instruction = 0xFF800000;
const UPPER_IMM_BRANCH_SHIFT: Instruction = 18; // Keep the lower bits in here free for the lower bit

// Shift/IO
const SMALL_IMM_MASK: Instruction = 0x003C0000;
const SMALL_IMM_SHIFT: Instruction = RS2_REG_SHIFT;

// Misc
const LI_IMM_UPPER_MASK: Instruction = 0x00030000;
const LI_IMM_UPPER_SHIFT: Instruction = 2;
//@formatter:on

const BRANCH_CODE: u8 = 4;

pub fn parse(instruction: Instruction) -> InstructionInfo {
	let mut ii = InstructionInfo::default();

	// Hlt -> all 0 anyway, just return now
	if instruction == 0 {
		return ii;
	}

	ii.opcode = (instruction & 0xff) as u8; // first 8-bits in the inst

	let upper_opcode = (ii.opcode & UPPER_OP_MASK) >> 5; // top 3-bits of the opcode
	let main_opcode = ii.opcode & MAIN_OP_MASK; // first 4-bits of the opcode
	let is_imm = ii.opcode & IS_IMM_MASK != 0;

	let is_branch_command = main_opcode == BRANCH_CODE;

	// Technically IN also doesn't have rs1, but at the same time it will always have a value of 0
	// so it doesn't matter. These other ones don't have it, but do have some imm value in it
	let has_rs1 = ii.opcode != OPCODE_MAP.get("li").unwrap().1 as u8
		&& ii.opcode != OPCODE_MAP.get("jal").unwrap().1 as u8;

	// If it is not an immediate, and it's not JAL, JALR, STR, LOD, IN, or OUT (`>= get('jal')`)
	// as branching does have rs2 ... handily everything with a main op >= 5 doesn't have rs2
	// I didn't even try to do that ... lol
	let has_rs2 = ii.opcode & (MAIN_OP_MASK | IS_IMM_MASK) < 5;

	if has_rs1 {
		ii.reg1 = ((instruction & RS1_REG_MASK) >> RS1_REG_SHIFT) as u8;
	} else {
		// Now has some imm value in here
		// Be safe and don't do weird things -- use unsigned
		// Puts the imm values into the top 2 bits, since that's where they need to go eventually
		ii.immediate |= ((instruction & LI_IMM_UPPER_MASK) >> LI_IMM_UPPER_SHIFT) as UnsignedWord;
	}

	if has_rs2 {
		ii.reg2 = ((instruction & RS2_REG_MASK) >> RS2_REG_SHIFT) as u8;
	}

	if !is_branch_command {
		ii.write_back_reg = ((instruction & RD_REG_MASK) >> RD_REG_SHIFT) as u8;
		ii.immediate |= ((instruction & LOWER_IMM_BRANCH_MASK) >> LOWER_IMM_BRANCH_SHIFT) as UnsignedWord;
		ii.immediate |= ((instruction & UPPER_IMM_BRANCH_MASK) >> UPPER_IMM_BRANCH_SHIFT) as UnsignedWord;
	} else {}


	;
	ii
}