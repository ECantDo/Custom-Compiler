use crate::cpu::RAM_SIZE;
use inkwell::module::Module;
use inkwell::values::{AnyValue, BasicValueEnum, FunctionValue, InstructionOpcode, InstructionValue};
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

fn apply_immediate_operation<'ctx>(
	registers: &mut RegAlloc<'ctx>,
	instruction: InstructionValue<'ctx>,
	op_name: &str) -> String {
	let lhs = instruction.get_operand(0).unwrap().unwrap_value();
	let rhs = instruction.get_operand(1).unwrap().unwrap_value();

	let mut out = String::new();

	let lhs_reg = resolve_operand(registers, &mut out, lhs);
	let dest = registers.get_or_assign(instruction.as_any_value_enum().try_into().unwrap());

	if let BasicValueEnum::IntValue(rhs_iv) = rhs {
		if rhs_iv.is_const() {
			if let Some(imm) = rhs_iv.get_sign_extended_constant() {
				if op_name == "sl" || op_name == "sr" || op_name == "sra" {
					if imm >= 16 {
						eprintln!("warning: shift amount {imm} exceeds 4-bit shifter width, will truncate to {}", imm & 0xF)
					} else if imm < 0 {
						eprintln!("warning: shift amount {imm} is less than 0, will truncate to {}", imm & 0xF)
					}
				}
				if imm >= -(IMMEDIATE_MAX_VALUE_14 + 1) && imm <= IMMEDIATE_MAX_VALUE_14  {
					out.push_str(&format!("\t{op_name}i\t{lhs_reg} {imm} {dest}\n"));
					return out;
				}
			}
		}
	}

	let rhs_reg = resolve_operand(registers, &mut out, rhs);
	out.push_str(&format!("\t{op_name}\t{lhs_reg} {rhs_reg} {dest}\n"));

	out
}

fn match_opcode<'ctx>(
	instruction: InstructionValue<'ctx>,
	registers: &mut RegAlloc<'ctx>,
	fn_name: &str,
	next_block_name: Option<&str>,
	is_main: bool,
) -> String {
	match instruction.get_opcode() {
		InstructionOpcode::Add => {
			apply_immediate_operation(registers, instruction, "add")
		}

		InstructionOpcode::Sub => {
			apply_immediate_operation(registers, instruction, "sub")
		}

		InstructionOpcode::Xor => {
			apply_immediate_operation(registers, instruction, "xor")
		}

		InstructionOpcode::Or => {
			apply_immediate_operation(registers, instruction, "or")
		}

		InstructionOpcode::And => {
			apply_immediate_operation(registers, instruction, "and")
		}

		InstructionOpcode::Shl => {
			apply_immediate_operation(registers, instruction, "sl")
		}

		InstructionOpcode::LShr => {
			apply_immediate_operation(registers, instruction, "sr")
		}

		InstructionOpcode::AShr => {
			apply_immediate_operation(registers, instruction, "sra")
		}

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
			let else_bb = instruction.get_operand(1).unwrap().unwrap_block();
			let then_name = then_bb.get_name().to_str().unwrap().to_string();
			let else_name = else_bb.get_name().to_str().unwrap().to_string();

			let predicate = icmp.get_icmp_predicate()
				.unwrap_or_else(|| panic!("Expected an icmp predicate, but found None"));

			if next_block_name == Some(then_name.as_str()) {
				// then_bb is the natural fallthrough - branch on the INVERTED
				// condition straight to else_bb instead, and emit nothing for
				// then_bb at all (it's reached just by falling through).
				let mnemonic = branch_mnemonic(invert_predicate(predicate));
				out.push_str(&format!("\t{mnemonic}\t{lhs_reg} {rhs_reg} {fn_name}_{else_name}\n"));
			} else {
				let mnemonic = branch_mnemonic(predicate);
				out.push_str(&format!("\t{mnemonic}\t{lhs_reg} {rhs_reg} {fn_name}_{then_name}\n"));
				if next_block_name != Some(else_name.as_str()) {
					out.push_str(&format!("\tj\t{fn_name}_{else_name}\n"));
				}
			}

			out
		}

		InstructionOpcode::Return => {
			let mut out = String::new();
			if let Some(val) = instruction.get_operand(0) {
				let val_reg = resolve_operand(registers, &mut out, val.unwrap_value());
				out.push_str(&format!("\tmv\t{val_reg} a0\n"));
			}
			// main returning isn't "return to a caller" - there is no caller.
			// It's the program ending.
			out.push_str(if is_main { "\thlt\n" } else { "\tret\n" });

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
	// Initialize stack pointer
	out.push_str(&format!("li\t{} sp\n", RAM_SIZE - 1));

	// Program entry point: jump straight to main regardless of what order
	// functions happen to be emitted in. If main is emitted first, the
	// peephole pass below strips this back out on its own (fallthrough and
	// jump-to-immediately-next-label are equivalent), so no special-casing
	// needed either way.
	if module.get_function("main").is_some() {
		out.push_str("j\tmain\n");
	}

	for function in module.get_functions() {
		let name = function.get_name().to_str().unwrap();
		let is_main = name == "main";
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

		coalesce_phi_copies(function, &mut registers);

		let blocks: Vec<_> = function.get_basic_block_iter().collect();
		for (i, block) in blocks.iter().enumerate() {
			let block_name = block.get_name().to_str().unwrap();
			if !block_name.is_empty() && block_name != "entry" {
				out.push_str(&format!("{name}_{block_name}:\n"));
			}

			let next_block_name = blocks.get(i + 1)
				.and_then(|b| b.get_name().to_str().ok())
				.filter(|s| !s.is_empty())
				.map(|s| s.to_string());

			let instructions: Vec<InstructionValue> = block.get_instructions().collect();

			for instruction in &instructions {
				if instruction.get_opcode() == InstructionOpcode::Phi { continue; }
				if instruction.is_terminator() {
					out.push_str(&emit_phi_moves(*instruction, block, &mut registers));
				}
				out.push_str(&match_opcode(*instruction, &mut registers, &name, next_block_name.as_deref(), is_main));
			}
		}
	}

	peephole_remove_redundant_jumps(&out)
	// out
}

fn peephole_remove_redundant_jumps(asm: &str) -> String {
	let lines: Vec<&str> = asm.lines().collect();
	let mut out = Vec::new();

	let mut i = 0;
	while i < lines.len() {
		let line = lines[i];
		if let Some(target) = line.trim().strip_prefix("j\t") {
			if let Some(next) = lines.get(i + 1) {
				if next.trim_end() == format!("{}:", target.trim()) {
					i += 1; // skip the jump, keep the label
					continue;
				}
			}
		}
		out.push(line);
		i += 1;
	}

	out.join("\n")
}

fn resolve_operand<'ctx>(regs: &mut RegAlloc<'ctx>, out: &mut String, val: BasicValueEnum<'ctx>) -> String {
	if let Some(const_val) = as_constant_i64(val) {
		if const_val == 0 {
			regs.assigned.insert(val, "zero".to_string());
			return "zero".to_string();
		}
		let reg = format!("t{}", regs.next);
		regs.next += 1;
		regs.assigned.insert(val, reg.clone());
		out.push_str(&format!("\tli\t{const_val} {reg}\n"));
		return reg;
	}
	regs.resolve(val)
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
			terminator.get_operand(1).unwrap().unwrap_block(),
			terminator.get_operand(2).unwrap().unwrap_block(),
		],
		_ => return String::new(),
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

						if is_undef(incoming_val) {
							// This edge never actually supplies a real value for
							// this phi (e.g. a loop-local variable merged at the
							// loop header along the preheader edge, before the
							// loop has run once). Nothing meaningful to move -
							// dest is left whatever it already was.
							continue;
						}

						let const_val = as_constant_i64(incoming_val);

						if let Some(v) = const_val {
							out.push_str(&format!("\tli\t{v} {dest}\n"));
						} else {
							let src = resolve_operand(registers, &mut out, incoming_val);
							if src != dest {
								out.push_str(&format!("\tmv\t{src} {dest}\n"));
							}
						}
					}
				}
			}
		}
	}
	out
}

/// Counts how many times each SSA value is used as an operand anywhere in
/// the function. A value can only be coalesced into a phi's register if
/// the phi is its ONLY use — if something else still needs it, giving it
/// away would just be wrong.
fn count_operand_uses<'ctx>(function: FunctionValue<'ctx>) -> HashMap<BasicValueEnum<'ctx>, usize> {
	let mut uses: HashMap<BasicValueEnum<'ctx>, usize> = HashMap::new();
	for block in function.get_basic_block_iter() {
		for instr in block.get_instructions() {
			for i in 0..instr.get_num_operands() {
				if let Some(Some(val)) = instr.get_operand(i).map(|op| op.value()) {
					*uses.entry(val).or_insert(0) += 1;
				}
			}
		}
	}
	uses
}

/// PASS 1.5: copy coalescing.
///
/// For every phi, look at the value flowing in from each predecessor. If
/// that value is computed in the predecessor block, used nowhere else,
/// AND doesn't itself depend on some OTHER phi in the same target block
/// (which would make this part of a swap/rotation - unsafe to coalesce),
/// bind it straight to the phi's register now. Codegen picks this up
/// later via `get_or_assign` and the `mv` this would've needed collapses
/// to a no-op.
fn coalesce_phi_copies<'ctx>(function: FunctionValue<'ctx>, registers: &mut RegAlloc<'ctx>) {
	let uses = count_operand_uses(function);

	for block in function.get_basic_block_iter() {
		let Some(terminator) = block.get_terminator() else { continue; };

		let targets: Vec<_> = match terminator.get_opcode() {
			InstructionOpcode::Br if terminator.get_num_operands() == 1 => {
				vec![terminator.get_operand(0).unwrap().unwrap_block()]
			}
			InstructionOpcode::Br => vec![
				terminator.get_operand(1).unwrap().unwrap_block(),
				terminator.get_operand(2).unwrap().unwrap_block(),
			],
			_ => continue,
		};

		for target in targets {
			let phis: Vec<InstructionValue> = target.get_instructions()
				.take_while(|i| i.get_opcode() == InstructionOpcode::Phi)
				.collect();

			for &phi_instr in &phis {
				let phi: inkwell::values::PhiValue = phi_instr.try_into().unwrap();

				for i in 0..phi.count_incoming() {
					let Some((incoming_val, incoming_block)) = phi.get_incoming(i) else { continue; };
					if incoming_block != block { continue; }

					// Only real instructions can be re-homed — constants get
					// materialized fresh at each use anyway.
					let BasicValueEnum::IntValue(iv) = incoming_val else { continue; };
					let Some(def_instr) = iv.as_instruction() else { continue; };

					if def_instr.get_parent() != Some(block) { continue; }
					if uses.get(&incoming_val).copied().unwrap_or(0) != 1 { continue; }
					if registers.assigned.contains_key(&incoming_val) { continue; }

					// Safety check: does this value depend on a SIBLING phi
					// (a different loop variable)? If so it's part of a
					// rotation/swap and needs its own temp - skip it.
					let depends_on_sibling_phi = (0..def_instr.get_num_operands()).any(|i| {
						def_instr.get_operand(i)
							.and_then(|op| op.value())
							.map(|val| phis.iter().any(|&p| {
								p != phi_instr && BasicValueEnum::try_from(p.as_any_value_enum()).unwrap() == val
							}))
							.unwrap_or(false)
					});
					if depends_on_sibling_phi { continue; }

					let phi_val: BasicValueEnum = phi_instr.as_any_value_enum().try_into().unwrap();
					let phi_reg = registers.resolve(phi_val);
					registers.bind(incoming_val, &phi_reg);
				}
			}
		}
	}
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

/// Returns the constant integer value of a BasicValueEnum, if it IS one —
/// covers plain int constants, null pointers, and non-null constant
/// pointers (inttoptr(N)). Returns None for anything that isn't a
/// compile-time constant.
fn as_constant_i64(val: BasicValueEnum) -> Option<i64> {
	match val {
		// NOTE: is_const() is true for constant EXPRESSIONS too (e.g. a
		// ptrtoint/mul chain folded by an optimization pass), not just plain
		// integer literals — get_sign_extended_constant() only succeeds for
		// the latter, so this must stay an `if let`, never a bare `.unwrap()`.
		BasicValueEnum::IntValue(iv) if iv.is_const() => {
			iv.get_sign_extended_constant()
		}
		BasicValueEnum::PointerValue(pv) if pv.is_null() => Some(0),
		BasicValueEnum::PointerValue(pv) if pv.is_const() => {
			let text = pv.print_to_string().to_string();
			text.split("inttoptr (")
				.nth(1)
				.and_then(|s| s.split_whitespace().nth(1))
				.and_then(|s| s.parse().ok())
		}
		_ => None,
	}
}
/// Whether a value is LLVM's `undef` — a value that's compile-time constant
/// in the broad sense but has no defined bit pattern (nothing to move into
/// a register). Distinct from a normal zero/null constant.
fn is_undef(val: BasicValueEnum) -> bool {
	match val {
		BasicValueEnum::IntValue(iv) => iv.is_undef(),
		BasicValueEnum::PointerValue(pv) => pv.is_undef(),
		BasicValueEnum::FloatValue(fv) => fv.is_undef(),
		BasicValueEnum::ArrayValue(av) => av.is_undef(),
		BasicValueEnum::StructValue(sv) => sv.is_undef(),
		BasicValueEnum::VectorValue(vv) => vv.is_undef(),
		_ => { panic!("Unsupported `is_undef` {val}") }
	}
}

fn branch_mnemonic(predicate: IntPredicate) -> &'static str {
	match predicate {
		IntPredicate::EQ => "beq",
		IntPredicate::NE => "bne",
		IntPredicate::SGT => "bgt",  // Signed Greater Than
		IntPredicate::SGE => "bge",  // Signed Greater Than or Equal
		IntPredicate::SLT => "blt",  // Signed Less Than
		IntPredicate::SLE => "ble",  // Signed Less Than or Equal
		IntPredicate::UGT => "bgtu", // Unsigned Higher
		IntPredicate::UGE => "bgeu", // Unsigned Higher or Same
		IntPredicate::ULT => "bltu", // Unsigned Lower
		IntPredicate::ULE => "bleu", // Unsigned Lower or Same
	}
}

/// Logical negation of a predicate - used when the "then" block is the
/// natural fallthrough, so we can branch directly to "else" on the opposite
/// condition instead of emitting a dead extra jump.
fn invert_predicate(predicate: IntPredicate) -> IntPredicate {
	match predicate {
		IntPredicate::EQ => IntPredicate::NE,
		IntPredicate::NE => IntPredicate::EQ,
		IntPredicate::SGT => IntPredicate::SLE,
		IntPredicate::SGE => IntPredicate::SLT,
		IntPredicate::SLT => IntPredicate::SGE,
		IntPredicate::SLE => IntPredicate::SGT,
		IntPredicate::UGT => IntPredicate::ULE,
		IntPredicate::UGE => IntPredicate::ULT,
		IntPredicate::ULT => IntPredicate::UGE,
		IntPredicate::ULE => IntPredicate::UGT,
	}
}