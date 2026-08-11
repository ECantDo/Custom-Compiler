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

// =================================================================================================
// SUCC
// =================================================================================================
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

// =================================================================================================
// Def/Uses
// =================================================================================================
pub struct DefUseEntry<'ctx> {
	pub uses: HashSet<BasicValueEnum<'ctx>>,
	pub def: Option<BasicValueEnum<'ctx>>,
}

fn compute_def_uses<'ctx>(instr: InstructionValue<'ctx>) -> DefUseEntry<'ctx> {
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

	DefUseEntry { uses, def }
}

// =================================================================================================
// Block
// =================================================================================================
struct BlockSummary<'ctx> {
	block_use: HashSet<BasicValueEnum<'ctx>>,
	block_def: HashSet<BasicValueEnum<'ctx>>,
	succ_blocks: Vec<BasicBlock<'ctx>>,
}

fn summarize_block<'ctx>(
	block: BasicBlock<'ctx>,
	def_use_table: &HashMap<InstructionValue<'ctx>, DefUseEntry<'ctx>>,
	succ_table: &HashMap<InstructionValue<'ctx>, SuccEntry<'ctx>>,
) -> BlockSummary<'ctx> {
	let mut block_use = HashSet::new();
	let mut block_def: HashSet<BasicValueEnum<'ctx>> = HashSet::new();

	for instr in block.get_instructions() {
		let entry = &def_use_table[&instr];

		// A use only counts toward this block's inputs if we haven't
		// already defined that value earlier in this same block.
		for &val in &entry.uses {
			if !block_def.contains(&val) {
				block_use.insert(val);
			}
		}

		if let Some(def_val) = entry.def {
			block_def.insert(def_val);
		}
	}

	// Successor BLOCKS come from the terminator's successor INSTRUCTIONS —
	// each successor instruction is the first instruction of its block.
	let terminator = block.get_terminator().expect("every block has a terminator");
	let succ_blocks = succ_table[&terminator]
		.iter()
		.map(|succ_instr| succ_instr.get_parent().expect("instruction belongs to a block"))
		.collect();

	BlockSummary { block_use, block_def, succ_blocks }
}
// =================================================================================================
// Compute
// =================================================================================================

fn initialize_liveness<'ctx>(function: FunctionValue<'ctx>) -> (LivenessInfo<'ctx>, HashMap<InstructionValue<'ctx>, DefUseEntry<'ctx>>) {
	let mut succ_table: HashMap<InstructionValue<'ctx>, SuccEntry<'ctx>> = HashMap::new();
	let mut def_use_table: HashMap<InstructionValue<'ctx>, DefUseEntry<'ctx>> = HashMap::new();

	for block in function.get_basic_block_iter() {
		for instruction in block.get_instructions() {
			succ_table.insert(instruction, compute_succ(instruction));
			def_use_table.insert(instruction, compute_def_uses(instruction));
		}
	}

	for block in function.get_basic_block_iter() {
		let phis = block.get_instructions().take_while(|i| i.get_opcode() == InstructionOpcode::Phi);
		for phi_instr in phis {
			let phi: inkwell::values::PhiValue = phi_instr.try_into().unwrap();
			for i in 0..phi.count_incoming() {
				let Some((incoming_val, incoming_block)) = phi.get_incoming(i) else { continue; };
				let Some(terminator) = incoming_block.get_terminator() else { continue; };
				def_use_table.entry(terminator)
					.or_insert_with(|| compute_def_uses(terminator))
					.uses.insert(incoming_val);
			}
		}
	}

	// Build per-block summaries (now that phi-adjusted uses are in def_use_table).
	let summaries: HashMap<BasicBlock<'ctx>, BlockSummary<'ctx>> = function
		.get_basic_block_iter()
		.map(|b| (b, summarize_block(b, &def_use_table, &succ_table)))
		.collect();

	let mut live_in: HashMap<BasicBlock<'ctx>, HashSet<BasicValueEnum<'ctx>>> = HashMap::new();
	let mut live_out: HashMap<BasicBlock<'ctx>, HashSet<BasicValueEnum<'ctx>>> = HashMap::new();
	for &block in summaries.keys() {
		live_in.insert(block, HashSet::new());
		live_out.insert(block, HashSet::new());
	}

	// Fixed-point iteration.
	loop {
		let mut changed = false;

		for (&block, summary) in &summaries {
			let mut new_out = HashSet::new();
			for &succ in &summary.succ_blocks {
				new_out.extend(live_in[&succ].iter().copied());
			}

			let mut new_in = new_out.difference(&summary.block_def).copied().collect::<HashSet<_>>();
			new_in.extend(summary.block_use.iter().copied());

			if new_out != live_out[&block] {
				live_out.insert(block, new_out);
				changed = true;
			}
			if new_in != live_in[&block] {
				live_in.insert(block, new_in);
				changed = true;
			}
		}

		if !changed {
			break;
		}
	}

	(LivenessInfo { live_in, live_out }, def_use_table)
}
pub struct InstructionLiveness<'ctx> {
	pub live_in: HashMap<InstructionValue<'ctx>, HashSet<BasicValueEnum<'ctx>>>,
	pub live_out: HashMap<InstructionValue<'ctx>, HashSet<BasicValueEnum<'ctx>>>,
}

fn refine_block_liveness<'ctx>(
	function: FunctionValue<'ctx>,
	block_liveness: &LivenessInfo<'ctx>,
	def_use_table: &HashMap<InstructionValue<'ctx>, DefUseEntry<'ctx>>,
) -> InstructionLiveness<'ctx> {
	let mut live_in = HashMap::new();
	let mut live_out = HashMap::new();

	for block in function.get_basic_block_iter() {
		// Seed with this block's already-solved live_out from the fixed point.
		let mut current: HashSet<BasicValueEnum<'ctx>> = block_liveness.live_out[&block].clone();

		// Walk this ONE block backward — no iteration needed, the boundary
		// value (`current`, starting from live_out[block]) is already correct.
		let instrs: Vec<InstructionValue<'ctx>> = block.get_instructions().collect();
		for &instr in instrs.iter().rev() {
			live_out.insert(instr, current.clone());

			let entry = &def_use_table[&instr];
			if let Some(def_val) = entry.def {
				current.remove(&def_val);
			}
			current.extend(entry.uses.iter().copied());

			live_in.insert(instr, current.clone());
		}
	}

	InstructionLiveness { live_in, live_out }
}


pub fn compute_liveness<'ctx>(function: FunctionValue<'ctx>) -> InstructionLiveness<'ctx> {
	let (block_liveness, def_use_table) = initialize_liveness(function);
	refine_block_liveness(function, &block_liveness, &def_use_table)
}