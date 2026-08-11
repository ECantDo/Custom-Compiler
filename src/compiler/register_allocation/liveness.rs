use inkwell::basic_block::BasicBlock;
use inkwell::values::{AnyValue, BasicValueEnum, FunctionValue, InstructionOpcode, InstructionValue};
use std::collections::{HashMap, HashSet};

pub struct LivenessInfo<'ctx> {
	pub live_in: HashMap<BasicBlock<'ctx>, HashSet<BasicValueEnum<'ctx>>>,
	pub live_out: HashMap<BasicBlock<'ctx>, HashSet<BasicValueEnum<'ctx>>>,
}

impl<'ctx> Default for LivenessInfo<'ctx> {
	fn default() -> Self {
		Self { live_out: HashMap::default(), live_in: HashMap::default() }
	}
}

// SUCC
struct SuccEntry<'ctx> {
	len: usize,
	succ_locations: [Option<InstructionValue<'ctx>>; 2],
}

impl<'ctx> SuccEntry<'ctx> {
	fn new() -> Self {
		Self::default()
	}

	fn push(&mut self, iv: InstructionValue<'ctx>) {
		assert!(self.len < 2, "a block can have at most {} successors", 2);
		self.succ_locations[self.len] = Some(iv);
		self.len += 1;
	}

	fn len(&self) -> usize {
		self.len
	}

	fn is_empty(&self) -> bool {
		self.len == 0
	}

	fn iter(&self) -> impl Iterator<Item = InstructionValue<'ctx>> + '_ {
		self.succ_locations[..self.len]
			.iter()
			.map(|opt| opt.expect("slots below len are always populated"))
	}
}

impl<'ctx> Default for SuccEntry<'ctx> {
	fn default() -> Self {
		Self { len: 0, succ_locations: [None, None] }
	}
}

impl<'ctx> IntoIterator for SuccEntry<'ctx> {
	type Item = InstructionValue<'ctx>;
	type IntoIter = std::iter::Flatten<std::array::IntoIter<Option<InstructionValue<'ctx>>, 2>>;

	fn into_iter(self) -> Self::IntoIter {
		self.succ_locations.into_iter().flatten()
	}
}

fn compute_succ<'ctx>(instr: InstructionValue<'ctx>) -> SuccEntry<'ctx> {
	let mut entry = SuccEntry::default();

	match instr.get_opcode() {
		InstructionOpcode::Return => {
			entry
		}

		InstructionOpcode::Br => {
			if instr.get_num_operands() == 1 {
				let target = instr.get_operand(0).unwrap().unwrap_block();
				entry.push(target.get_first_instruction().unwrap());
				entry
			} else {
				let else_bb = instr.get_operand(1).unwrap().unwrap_block();
				let then_bb = instr.get_operand(2).unwrap().unwrap_block();
				entry.push(else_bb.get_first_instruction().unwrap());
				entry.push(then_bb.get_first_instruction().unwrap());
				entry
			}
		}

		// Other instruction, falls through
		_ => {
			match instr.get_next_instruction() {
				Some(next) => {
					entry.push(next);
					entry
				}
				None => entry, // Shouldn't happen
			}
		}
	}
}
struct DefEntry<'ctx> {
	uses: HashSet<BasicValueEnum<'ctx>>,
	def: Option<BasicValueEnum<'ctx>>,
}

fn compute_def<'ctx>(instr: InstructionValue<'ctx>) -> DefEntry<'ctx> {
	let mut uses = HashSet::new();

	if instr.get_opcode() != InstructionOpcode::Phi {
		for i in 0..instr.get_num_operands() {
			if let Some(operand) = instr.get_operand(i) {
				if let Some(val) = operand.value() {
					uses.insert(val);
				}
			}
		}
	}

	let def: Option<BasicValueEnum<'ctx>> = if instr.get_type().is_void_type() {
		None
	} else {
		Some(instr.as_any_value_enum().try_into().unwrap())
	};

	DefEntry { uses, def }
}


fn compute_liveness<'ctx>(function: FunctionValue<'ctx>) -> LivenessInfo<'ctx> {
	// Tables :3
	let mut succ_table: HashMap<InstructionValue<'ctx>, SuccEntry<'ctx>> = HashMap::new();
	let mut def_table: HashMap<InstructionValue<'ctx>, DefEntry<'ctx>> = HashMap::new();


	let mut liveness_info = LivenessInfo::default();

	// Step 1. Populate `use`, `def`, and `succ`
	for block in function.get_basic_block_iter() {
		for instruction in block.get_instructions() {
			succ_table.insert(instruction, compute_succ(instruction));
			def_table.insert(instruction, compute_def(instruction));
		}
	}

	liveness_info
}