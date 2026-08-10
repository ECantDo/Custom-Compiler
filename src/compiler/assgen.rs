use inkwell::module::Module;
use inkwell::values::{AnyValue, BasicValueEnum, InstructionOpcode, InstructionValue};
use inkwell::IntPredicate;
use std::collections::HashMap;

struct RegAlloc<'ctx> {
	next: usize,                                          // Index in to t0...t6 pool
	assigned: HashMap<BasicValueEnum<'ctx>, String>,    // LLVM value -> register name
}

impl<'ctx> RegAlloc<'ctx> {
	fn new() -> Self {
		RegAlloc { next: 0, assigned: HashMap::new() }
	}

	/// Params are pre-assigned to a0/a1/...  never allocate a fresh temp for them.
	fn bind(&mut self, val: BasicValueEnum<'ctx>, reg: &str) {
		self.assigned.insert(val, reg.to_string());
	}

	/// For instruction RESULTS; assigns the next free scratch reg if not seen yet.
	fn get_or_assign(&mut self, val: BasicValueEnum<'ctx>) -> String {
		if let Some(reg) = self.assigned.get(&val) {
			return reg.clone();
		}
		let reg = format!("t{}", self.next);
		self.next += 1;
		self.assigned.insert(val, reg.clone());
		reg
	}

	/// Looked-up-only version — panics if the value was never assigned, which
	/// should only happen if instruction selection missed emitting something.
	fn resolve(&self, val: BasicValueEnum<'ctx>) -> String {
		self.assigned.get(&val).cloned()
			.unwrap_or_else(|| panic!("value used before it was assigned a register"))
	}
}

fn match_opcode(instruction: InstructionValue, registers: &mut RegAlloc, fn_name: &str) -> String {
	match instruction.get_opcode() {
		InstructionOpcode::Load => {
			let addr = instruction.get_operand(0).unwrap().unwrap_value();
			let addr_reg = registers.resolve(addr);

			let dest = registers.get_or_assign(instruction.as_any_value_enum().try_into().unwrap());
			format!("\tlod\t{dest} 0{addr_reg}\n")
		}

		InstructionOpcode::Store => {
			let value = instruction.get_operand(0).unwrap().unwrap_value();
			let addr = instruction.get_operand(1).unwrap().unwrap_value();

			let mut out = String::new();

			let value_reg = resolve_operand(registers, &mut out, value);
			let addr_reg = resolve_operand(registers, &mut out, addr);

			out.push_str(&format!("\tlod\t{value_reg} 0({addr_reg})\n"));
			out
		}

		InstructionOpcode::Add => {
			let lhs = instruction.get_operand(0).unwrap().unwrap_value();
			let rhs = instruction.get_operand(1).unwrap().unwrap_value();

			let mut out = String::new();

			let lhs_reg = resolve_operand(registers, &mut out, lhs);
			let rhs_reg = resolve_operand(registers, &mut out, rhs);

			let dest = registers.get_or_assign(instruction.as_any_value_enum().try_into().unwrap());

			out.push_str(&format!("\tadd\t{lhs_reg} {rhs_reg} {dest}\n"));

			out
		}

		InstructionOpcode::ICmp => {
			let next = instruction.get_next_instruction();

			let fuses = matches!(next.map(|n| n.get_opcode()), Some(InstructionOpcode::Br));
			if !fuses {
				panic!("icmp not directly followed by a conditional br — not selected yet");
			}
			String::new()
		}

		InstructionOpcode::Br => {
			if instruction.get_num_operands() == 1 {
				// Unconditional
				let target = instruction.get_operand(0).unwrap().unwrap_block();
				format!("\tj\t{fn_name}_{}\n", target.get_name().to_str().unwrap());
			}

			let icmp = instruction.get_previous_instruction().unwrap();
			if icmp.get_opcode() != InstructionOpcode::ICmp {
				panic!("Conditional br without a preceding icmp");
			}

			let lhs = icmp.get_operand(0).unwrap().unwrap_value();
			let rhs = icmp.get_operand(1).unwrap().unwrap_value();

			let mut out = String::new();

			let lhs_reg = resolve_operand(registers, &mut out, lhs);
			let rhs_reg = resolve_operand(registers, &mut out, rhs);

			let then_bb = instruction.get_operand(2).unwrap().unwrap_block();

			let branch_type = match icmp.get_icmp_predicate() {
				Some(IntPredicate::EQ) => "beq",
				Some(IntPredicate::NE) => "bne",
				Some(IntPredicate::SGT) => "bgt", // Signed Greater Than
				Some(IntPredicate::SGE) => "bge", // Signed Greater Than or Equal
				Some(IntPredicate::SLT) => "blt", // Signed Less Than
				Some(IntPredicate::SLE) => "ble", // Signed Less Than or Equal

				// Does not exist...
				Some(IntPredicate::UGT) => "hi", // Unsigned Higher
				Some(IntPredicate::UGE) => "hs", // Unsigned Higher or Same
				Some(IntPredicate::ULT) => "lo", // Unsigned Lower
				Some(IntPredicate::ULE) => "ls", // Unsigned Lower or Same
				None => panic!("Expected an icmp predicate, but found None"),
			};

			out.push_str(&format!("\t{branch_type}\t{lhs_reg} {rhs_reg} {fn_name}_{}",
								  then_bb.get_name().to_str().unwrap()));

			out
		}

		InstructionOpcode::Return => {
			let mut out = String::new();
			if let Some(val) = instruction.get_operand(0) {
				let val_reg = resolve_operand(registers, &mut out, val.unwrap_value());
				out.push_str(&format!("\tmv\t{val_reg} a0\n"));
			}
			out.push_str("ret");

			out
		}

		other => panic!("instruction not selected yet: {:?}", other),
	}
}


pub fn build(module: &Module) -> String {
	let mut out = String::new();

	for function in module.get_functions() {
		let name = function.get_name().to_str().unwrap();
		out.push_str(&format!("{name}:\n"));

		let mut registers = RegAlloc::new();
		// Bind a0 ... regs
		for (i, param) in function.get_param_iter().enumerate() {
			registers.bind(param, &format!("a{i}"));
		}

		for block in function.get_basic_block_iter() {
			let block_name = block.get_name().to_str().unwrap();
			if !block_name.is_empty() && block_name != "entry" {
				out.push_str(&format!("{name}_{block_name}:\n"));
			}

			let instructions: Vec<InstructionValue> = block.get_instructions().collect();
			let mut i = 0;
			while i < instructions.len() {
				let instruction = instructions[i];

				out.push_str(&match_opcode(instruction, &mut registers, &name));

				i += 1;
			}
		}
	}

	return out;
}

/// Resolves an operand to a register name — if it's a known value (param or
/// prior instruction result), returns its register. If it's a constant,
/// emits an `li` into a fresh scratch register first, then returns that.
fn resolve_operand<'ctx>(regs: &mut RegAlloc<'ctx>, out: &mut String, val: BasicValueEnum<'ctx>) -> String {
	if let BasicValueEnum::IntValue(iv) = val {
		if iv.is_const() {
			let const_val = iv.get_sign_extended_constant().unwrap();
			let reg = format!("t{}", regs.next);
			regs.next += 1;
			regs.assigned.insert(val, reg.clone());
			out.push_str(&format!("\tli\t{reg}, {const_val}\n"));
			return reg;
		}
	}
	regs.resolve(val)
}