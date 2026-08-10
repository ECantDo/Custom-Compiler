use inkwell::module::Module;
use inkwell::values::{AnyValue, BasicValueEnum, InstructionOpcode, InstructionValue};
use inkwell::IntPredicate;
use std::collections::HashMap;

const IMMEDIATE_MAX_VALUE_14: i64 = 0x1FFF; // 13 bits

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
			.unwrap_or_else(|| panic!("value used before it was assigned a register\n{}", val))
	}
}

fn match_opcode<'ctx>(
	instruction: InstructionValue<'ctx>,
	registers: &mut RegAlloc<'ctx>,
	fn_name: &str,
) -> String {
	match instruction.get_opcode() {
		InstructionOpcode::Load => {
			let addr = instruction.get_operand(0).unwrap().unwrap_value();


			let mut out = String::new();
			let (base_reg, offset) = resolve_address(registers, &mut out, addr);

			let dest = registers.get_or_assign(instruction.as_any_value_enum().try_into().unwrap());
			out.push_str(&format!("\tlod\t{dest} {offset}({base_reg})\n"));
			out
		}

		InstructionOpcode::Store => {
			let value = instruction.get_operand(0).unwrap().unwrap_value();
			let addr = instruction.get_operand(1).unwrap().unwrap_value();

			let mut out = String::new();
			let value_reg = resolve_operand(registers, &mut out, value);
			let (base_reg, offset) = resolve_address(registers, &mut out, addr);

			out.push_str(&format!("\tstr\t{value_reg} {offset}({base_reg})\n"));
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
				return format!("\tj\t{fn_name}_{}\n", target.get_name().to_str().unwrap());
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
			out.push_str("\tret\n");

			out
		}

		InstructionOpcode::ZExt => {
			let src = instruction.get_operand(0).unwrap().unwrap_value();
			let mut out = String::new();
			let src_reg = resolve_operand(registers, &mut out, src);

			// Both source and destination fit in a single 16-bit word ... no real
			// instruction needed, just alias the destination to the same register.
			// NOTE: this assumes dest fits in ≤16 bits. Zero-extending into a real
			// 32-bit value (two registers, low+high) isn't handled yet, that needs
			// ADDF/SUBF two-register i32 convention wired in here too
			registers.assigned.insert(instruction.as_any_value_enum().try_into().unwrap(), src_reg);
			out
		}

		InstructionOpcode::IntToPtr => {
			let src = instruction.get_operand(0).unwrap().unwrap_value();
			let mut out = String::new();
			let src_reg = resolve_operand(registers, &mut out, src);

			// On this hardware a pointer IS just a 16-bit address — inttoptr is a
			// pure reinterpretation, not a real operation. Alias dest to src's reg.
			registers.assigned.insert(instruction.as_any_value_enum().try_into().unwrap(), src_reg);
			out
		}

		InstructionOpcode::PtrToInt => {
			let src = instruction.get_operand(0).unwrap().unwrap_value();
			let mut out = String::new();
			let src_reg = resolve_operand(registers, &mut out, src);
			registers.assigned.insert(instruction.as_any_value_enum().try_into().unwrap(), src_reg);
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

		// PASS 1: pre-assign registers to every phi in the function
		for block in function.get_basic_block_iter() {
			for instr in block.get_instructions() {
				if instr.get_opcode() == InstructionOpcode::Phi {
					registers.get_or_assign(instr.as_any_value_enum().try_into().unwrap());
				} else {
					break; // phis always sit at the very top of a block
				}
			}
		}

		for block in function.get_basic_block_iter() {
			let block_name = block.get_name().to_str().unwrap();
			if !block_name.is_empty() && block_name != "entry" {
				out.push_str(&format!("{name}_{block_name}:\n"));
			}

			let instructions: Vec<InstructionValue> = block.get_instructions().collect();

			for instruction in &instructions {
				if instruction.get_opcode() == InstructionOpcode::Phi { continue; }
				if instruction.is_terminator() {
					out.push_str(&emit_phi_moves(*instruction, &block, &mut registers));
				}
				out.push_str(&match_opcode(*instruction, &mut registers, &name));
			}
		}
	}

	out
}

/// Resolves an operand to a register name — if it's a known value (param or
/// prior instruction result), returns its register. If it's a constant,
/// emits an `li` into a fresh scratch register first, then returns that.
fn resolve_operand<'ctx>(regs: &mut RegAlloc<'ctx>, out: &mut String, val: BasicValueEnum<'ctx>) -> String {
	match val {
		BasicValueEnum::IntValue(iv) if iv.is_const() => {
			let const_val = iv.get_sign_extended_constant().unwrap();
			if const_val == 0 {
				regs.assigned.insert(val, "zero".to_string());
				return "zero".to_string();
			}
			let reg = format!("t{}", regs.next);
			regs.next += 1;
			regs.assigned.insert(val, reg.clone());
			out.push_str(&format!("\tli\t{reg}, {const_val}\n"));
			reg
		}
		BasicValueEnum::PointerValue(pv) if pv.is_null() => {
			// address 0 as a pointer
			regs.assigned.insert(val, "zero".to_string());
			"zero".to_string()
		}
		BasicValueEnum::PointerValue(pv) if pv.is_const() => {
			let text = pv.print_to_string().to_string();
			// text looks like: "ptr inttoptr (i16 1 to ptr)" — pull the integer out
			let const_val: i64 = text
				.split("inttoptr (")
				.nth(1)
				.and_then(|s| s.split_whitespace().nth(1))
				.and_then(|s| s.parse().ok())
				.unwrap_or_else(|| panic!("couldn't parse constant pointer text: {text}"));

			let reg = format!("t{}", regs.next);
			regs.next += 1;
			regs.assigned.insert(val, reg.clone());
			out.push_str(&format!("\tli\t{reg}, {const_val}\n"));
			reg
		}
		_ => regs.resolve(val),
	}
}

fn emit_phi_moves<'ctx>(
	terminator: InstructionValue<'ctx>,
	current_block: &inkwell::basic_block::BasicBlock<'ctx>,
	registers: &mut RegAlloc<'ctx>,
) -> String {
	let targets: Vec<_> = match terminator.get_opcode() {
		InstructionOpcode::Br if terminator.get_num_operands() == 1 => {
			vec![terminator.get_operand(0).unwrap().unwrap_block()]
		}
		InstructionOpcode::Br => vec![
			terminator.get_operand(1).unwrap().unwrap_block(), // false target
			terminator.get_operand(2).unwrap().unwrap_block(), // true target
		],
		_ => return String::new(), // Return has no successors, nothing to do
	};

	let mut out = String::new();
	for target in targets {
		for instr in target.get_instructions() {
			if instr.get_opcode() != InstructionOpcode::Phi { break; }
			let phi: inkwell::values::PhiValue = instr.try_into().unwrap();
			for i in 0..phi.count_incoming() {
				if let Some((incoming_val, incoming_block)) = phi.get_incoming(i) {
					if incoming_block == *current_block {
						let dest = registers.get_or_assign(instr.as_any_value_enum().try_into().unwrap());
						let src = resolve_operand(registers, &mut out, incoming_val);
						if src != dest {
							out.push_str(&format!("\tmv\t{src} {dest}\n"));
						}
					}
				}
			}
		}
	}
	out
}

/// Splits an address operand into (base register, constant offset).
/// If it's just a plain value with no addition involved, offset is 0.
/// If it's `add base, CONST` where CONST is a compile-time constant,
/// folds CONST into the addressing mode instead of materializing it
/// as a separate `li` + register.
fn resolve_address<'ctx>(regs: &mut RegAlloc<'ctx>, out: &mut String, addr: BasicValueEnum<'ctx>) -> (String, i64) {
	// Case 1: address is a plain constant int (e.g. a folded `first_free`)
	if let BasicValueEnum::IntValue(iv) = addr {
		if iv.is_const() {
			let val = iv.get_sign_extended_constant().unwrap();
			if val >= -(IMMEDIATE_MAX_VALUE_14 + 1) && val <= IMMEDIATE_MAX_VALUE_14 {
				return ("zero".to_string(), val);
			}
		}
	}

	// Case 2: address is a constant pointer (null, or inttoptr(N))
	if let BasicValueEnum::PointerValue(pv) = addr {
		if pv.is_null() {
			return ("zero".to_string(), 0);
		}
		if pv.is_const() {
			let text = pv.print_to_string().to_string();
			if let Some(val) = text.split("inttoptr (").nth(1)
				.and_then(|s| s.split_whitespace().nth(1))
				.and_then(|s| s.parse::<i64>().ok())
			{
				if val >= -(IMMEDIATE_MAX_VALUE_14 + 1) && val <= IMMEDIATE_MAX_VALUE_14 {
					return ("zero".to_string(), val);
				}
			}
		}
	}

	// Case 3: address is `base + CONST` (existing add-folding logic)
	if let BasicValueEnum::IntValue(iv) = addr {
		if let Some(instr) = iv.as_instruction() {
			if instr.get_opcode() == InstructionOpcode::Add {
				let lhs = instr.get_operand(0).unwrap().unwrap_value();
				let rhs = instr.get_operand(1).unwrap().unwrap_value();
				if let BasicValueEnum::IntValue(rhs_iv) = rhs {
					if rhs_iv.is_const() {
						let offset = rhs_iv.get_sign_extended_constant().unwrap();
						if offset >= -(IMMEDIATE_MAX_VALUE_14 + 1) && offset <= IMMEDIATE_MAX_VALUE_14 {
							let base_reg = resolve_operand(regs, out, lhs);
							return (base_reg, offset);
						}
					}
				}
			}
		}
	}

	// Fallback: nothing folds, resolve the whole address as a normal value
	(resolve_operand(regs, out, addr), 0)
}