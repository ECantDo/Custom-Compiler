use crate::compiler::register_allocation::config::RegisterConfig;
use crate::compiler::register_allocation::{self, Coloring};
use crate::cpu::RAM_SIZE;
use inkwell::basic_block::BasicBlock;
use inkwell::module::Module;
use inkwell::values::{AnyValue, AsValueRef, BasicValueEnum, InstructionOpcode, InstructionValue};
use inkwell::IntPredicate;
use std::collections::{HashMap, HashSet};

const IMMEDIATE_MAX_VALUE_14: i64 = 0x1FFF; // 13 bits

/// Maps SSA values to concrete physical registers for one function.
///
/// The graph-colouring allocator (`register_allocation`) decides where every
/// instruction result lives; this just resolves lookups and hands out the
/// codegen scratch registers for things the allocator never sees (constants,
/// parallel-copy cycle breaks).
struct RegAlloc<'ctx> {
	/// SSA value -> register name. Seeded from the colouring, plus the
	/// parameter bindings (a0, a1, ...).
	assigned: HashMap<BasicValueEnum<'ctx>, String>,
	/// Scratch register names from the target config, e.g. t4/t5/t6.
	scratch: Vec<String>,
	/// How many scratch registers are live for the instruction currently
	/// being lowered. Reset via `scratch_reset` before each one.
	scratch_in_use: usize,
}

impl<'ctx> RegAlloc<'ctx> {
	fn new(coloring: &Coloring<'ctx>, config: &RegisterConfig) -> Self {
		let assigned = coloring
			.colors
			.keys()
			.map(|&value| {
				let reg = coloring
					.register_for(value, config)
					.expect("coloured value maps to an allocatable register");
				(value, reg)
			})
			.collect();

		let scratch = config.scratch_registers().map(|r| r.name.clone()).collect();

		RegAlloc { assigned, scratch, scratch_in_use: 0 }
	}

	/// Pin a value to a specific register outside the colouring - used for
	/// parameters, which live in their argument registers.
	fn bind(&mut self, val: BasicValueEnum<'ctx>, reg: &str) {
		self.assigned.insert(val, reg.to_string());
	}

	/// Register holding `val`. Panics if it was never assigned one, which
	/// means the allocator and instruction selection disagree about what
	/// needs a register.
	fn resolve(&self, val: BasicValueEnum<'ctx>) -> String {
		self.assigned.get(&val).cloned().unwrap_or_else(|| {
			panic!("value has no register (not coloured, not a bound parameter)\n{}", val)
		})
	}

	/// Forget any scratch registers handed out for the previous instruction.
	fn scratch_reset(&mut self) {
		self.scratch_in_use = 0;
	}

	/// Hand out the next free scratch register for the instruction being
	/// lowered. Valid only until the next `scratch_reset`.
	fn next_scratch(&mut self) -> String {
		let reg = self.scratch.get(self.scratch_in_use).unwrap_or_else(|| {
			panic!(
				"code generator ran out of scratch registers (only {} configured)",
				self.scratch.len()
			)
		});
		self.scratch_in_use += 1;
		reg.clone()
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
	let dest = registers.resolve(instruction.as_any_value_enum().try_into().unwrap());

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
				if imm >= -(IMMEDIATE_MAX_VALUE_14 + 1) && imm <= IMMEDIATE_MAX_VALUE_14 {
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

/// Emit the test for a conditional `br` as a single branch to `target`.
///
/// `taken_on_then` picks which edge the branch selects: `true` means the
/// branch is taken exactly when the THEN edge should be followed, `false`
/// means it is taken when the ELSE edge should be followed (the predicate is
/// inverted for that case).
fn emit_cond_branch<'ctx>(
	registers: &mut RegAlloc<'ctx>,
	out: &mut String,
	fused: Option<InstructionValue<'ctx>>,
	cond: BasicValueEnum<'ctx>,
	target: &str,
	taken_on_then: bool,
) {
	if let Some(cond_instr) = fused {
		let lhs = cond_instr.get_operand(0).unwrap().unwrap_value();
		let rhs = cond_instr.get_operand(1).unwrap().unwrap_value();
		let lhs_reg = resolve_operand(registers, out, lhs);
		let rhs_reg = resolve_operand(registers, out, rhs);
		let predicate = cond_instr.get_icmp_predicate().unwrap();
		let predicate = if taken_on_then { predicate } else { invert_predicate(predicate) };
		out.push_str(&format!("\t{}\t{lhs_reg} {rhs_reg} {target}\n", branch_mnemonic(predicate)));
	} else {
		// The condition is a materialised 0/1 value: take the branch when it
		// is non-zero (THEN) or zero (ELSE).
		let cond_reg = resolve_operand(registers, out, cond);
		let mnemonic = if taken_on_then { "bne" } else { "beq" };
		out.push_str(&format!("\t{mnemonic}\t{cond_reg} zero {target}\n"));
	}
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

			let dest = registers.resolve(instruction.as_any_value_enum().try_into().unwrap());
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
			if is_fused_into_next_br(instruction) {
				String::new() // handled by Br below, same as today
			} else {
				materialize_icmp(instruction, registers)
			}
		}

		InstructionOpcode::Br => {
			let mut out = String::new();
			let pred = instruction.get_parent().unwrap();

			// --- unconditional branch ------------------------------------
			if instruction.get_num_operands() == 1 {
				let target = instruction.get_operand(0).unwrap().unwrap_block();
				let target_name = target.get_name().to_str().unwrap().to_string();
				out.push_str(&phi_moves_for_edge(pred, target, registers));
				if next_block_name != Some(target_name.as_str()) {
					out.push_str(&format!("\tj\t{fn_name}_{target_name}\n"));
				}
				return out;
			}

			// --- conditional branch ------------------------------------
			let cond = instruction.get_operand(0).unwrap().unwrap_value();
			let then_bb = instruction.get_operand(2).unwrap().unwrap_block();
			let else_bb = instruction.get_operand(1).unwrap().unwrap_block();
			let then_name = then_bb.get_name().to_str().unwrap().to_string();
			let else_name = else_bb.get_name().to_str().unwrap().to_string();

			// Phi copies belong on their edge. Build them up front so we know
			// whether we can use the tight fall-through form or need a
			// per-edge trampoline.
			let then_moves = phi_moves_for_edge(pred, then_bb, registers);
			let else_moves = phi_moves_for_edge(pred, else_bb, registers);
			registers.scratch_reset();

			// Is the condition an icmp sitting right before us? Then we test
			// its operands directly instead of a materialised 0/1 value.
			let fused = match cond {
				BasicValueEnum::IntValue(iv) => iv.as_instruction().filter(|ci| {
					ci.get_opcode() == InstructionOpcode::ICmp
						&& instruction.get_previous_instruction() == Some(*ci)
				}),
				_ => None,
			};

			let then_label = format!("{fn_name}_{then_name}");
			let else_label = format!("{fn_name}_{else_name}");

			// No phi copies on either edge: a single conditional branch plus at
			// most one fall-through jump.
			if then_moves.is_empty() && else_moves.is_empty() {
				if next_block_name == Some(then_name.as_str()) {
					emit_cond_branch(registers, &mut out, fused, cond, &else_label, false);
				} else {
					emit_cond_branch(registers, &mut out, fused, cond, &then_label, true);
					if next_block_name != Some(else_name.as_str()) {
						out.push_str(&format!("\tj\t{else_label}\n"));
					}
				}
				return out;
			}

			// Only one edge carries copies: branch straight to the *other*
			// block, then fall through this edge's copies into its block. The
			// copies sit after the branch, so they run only when this edge is
			// taken - and can't clobber the branch operands.
			if else_moves.is_empty() {
				emit_cond_branch(registers, &mut out, fused, cond, &else_label, false);
				out.push_str(&then_moves);
				if next_block_name != Some(then_name.as_str()) {
					out.push_str(&format!("\tj\t{then_label}\n"));
				}
				return out;
			}
			if then_moves.is_empty() {
				emit_cond_branch(registers, &mut out, fused, cond, &then_label, true);
				out.push_str(&else_moves);
				if next_block_name != Some(else_name.as_str()) {
					out.push_str(&format!("\tj\t{else_label}\n"));
				}
				return out;
			}

			// Both edges carry copies. Emit a trampoline so each edge's copies
			// run only when that edge is taken:
			//
			//     <test>  branch to L_then when the THEN condition holds
			//     <else edge copies>
			//     j else
			//   L_then:
			//     <then edge copies>
			//     j then          (dropped when `then` is the next block)
			let id = instruction.as_value_ref() as usize;
			let trampoline = format!("{fn_name}_br{id}_then");
			emit_cond_branch(registers, &mut out, fused, cond, &trampoline, true);
			out.push_str(&else_moves);
			out.push_str(&format!("\tj\t{else_label}\n"));
			out.push_str(&format!("{trampoline}:\n"));
			out.push_str(&then_moves);
			if next_block_name != Some(then_name.as_str()) {
				out.push_str(&format!("\tj\t{then_label}\n"));
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
			let dest = registers.resolve(instruction.as_any_value_enum().try_into().unwrap());

			// Both source and destination fit in a single 16-bit word, so this
			// is just a copy into the destination register the allocator chose
			// (and folds away entirely when they coincide - see the self-move
			// peephole). NOTE: this assumes dest fits in ≤16 bits. Zero-extending
			// into a real 32-bit value (two registers, low+high) isn't handled
			// yet, that needs the ADDF/SUBF two-register i32 convention here too.
			if src_reg != dest {
				out.push_str(&format!("\tmv\t{src_reg} {dest}\n"));
			}
			out
		}

		InstructionOpcode::IntToPtr => {
			let src = instruction.get_operand(0).unwrap().unwrap_value();
			let mut out = String::new();
			let src_reg = resolve_operand(registers, &mut out, src);
			let dest = registers.resolve(instruction.as_any_value_enum().try_into().unwrap());

			// On this hardware a pointer IS just a 16-bit address — inttoptr is a
			// pure reinterpretation, not a real operation. Copy into dest (folds
			// away when src and dest share a register).
			if src_reg != dest {
				out.push_str(&format!("\tmv\t{src_reg} {dest}\n"));
			}
			out
		}

		InstructionOpcode::PtrToInt => {
			let src = instruction.get_operand(0).unwrap().unwrap_value();
			let mut out = String::new();
			let src_reg = resolve_operand(registers, &mut out, src);
			let dest = registers.resolve(instruction.as_any_value_enum().try_into().unwrap());
			if src_reg != dest {
				out.push_str(&format!("\tmv\t{src_reg} {dest}\n"));
			}
			out
		}

		InstructionOpcode::Select => {
			let cond = instruction.get_operand(0).unwrap().unwrap_value();
			let true_val = instruction.get_operand(1).unwrap().unwrap_value();
			let false_val = instruction.get_operand(2).unwrap().unwrap_value();

			let mut out = String::new();
			let cond_reg = resolve_operand(registers, &mut out, cond);
			let true_reg = resolve_operand(registers, &mut out, true_val);
			let false_reg = resolve_operand(registers, &mut out, false_val);
			let dest = registers.resolve(instruction.as_any_value_enum().try_into().unwrap());

			// Raw pointer as a cheap unique id for label naming - same trick as
			// materialize_icmp, works regardless of the select's result type.
			let id = instruction.as_value_ref() as usize;
			let end_label = format!("select_end_{id}");

			out.push_str(&format!("\tmv\t{false_reg} {dest}\n"));
			out.push_str(&format!("\tbeq\t{cond_reg} zero {end_label}\n"));
			out.push_str(&format!("\tmv\t{true_reg} {dest}\n"));
			out.push_str(&format!("{end_label}:\n"));

			out
		}

		other => panic!("instruction not selected yet: {:?}", other),
	}
}


pub fn build(module: &Module) -> String {
	let mut out = String::new();
	// Initialize stack pointer

	// Program entry point: jump straight to main regardless of what order
	// functions happen to be emitted in. If main is emitted first, the
	// peephole pass below strips this back out on its own (fallthrough and
	// jump-to-immediately-next-label are equivalent), so no special-casing
	// needed either way.
	if module.get_function("main").is_some() {
		out.push_str(&format!("li\t{} sp\n", RAM_SIZE - 1));
		out.push_str("j\tmain\n");
	}

	let config = RegisterConfig::load_default()
		.unwrap_or_else(|e| panic!("register allocation config: {e}"));

	for function in module.get_functions() {
		let name = function.get_name().to_str().unwrap();
		let is_main = name == "main";
		out.push_str(&format!("\n{name}:\n"));

		if function.count_basic_blocks() == 0 {
			continue; // extern declaration — no body
		}

		let coloring = register_allocation::allocate_function(function, &config)
			.unwrap_or_else(|e| panic!("{e}"));

		let mut registers = RegAlloc::new(&coloring, &config);
		// Parameters arrive in the argument registers a0, a1, ...
		for (param, arg_reg) in function.get_param_iter().zip(config.arg_registers()) {
			registers.bind(param, &arg_reg.name);
		}

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
				// Phi copies for the block's successors are emitted by the
				// terminator (Br) itself, placed on the correct edge.
				registers.scratch_reset();
				out.push_str(&match_opcode(*instruction, &mut registers, name, next_block_name.as_deref(), is_main));
			}
		}
	}

	peephole(&out)
}

/// Cheap cleanup pass over the finished assembly text:
///   * drop `mv x x` (the allocator put a copy's source and destination in
///     the same register - common for zext/inttoptr and coalesced phis);
///   * drop `j L` when `L:` is the very next line.
fn peephole(asm: &str) -> String {
	let kept: Vec<&str> = asm
		.lines()
		.filter(|line| {
			let trimmed = line.trim();
			if let Some(rest) = trimmed.strip_prefix("mv\t") {
				let mut parts = rest.split_whitespace();
				if let (Some(src), Some(dest), None) = (parts.next(), parts.next(), parts.next()) {
					return src != dest;
				}
			}
			true
		})
		.collect();

	let mut out = Vec::with_capacity(kept.len());
	let mut i = 0;
	while i < kept.len() {
		let line = kept[i];
		if let Some(target) = line.trim().strip_prefix("j\t") {
			if let Some(next) = kept.get(i + 1) {
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
			return "zero".to_string();
		}
		// Constants aren't register-allocated - materialise into a scratch
		// register, fresh at each use.
		let reg = regs.next_scratch();
		out.push_str(&format!("\tli\t{const_val} {reg}\n"));
		return reg;
	}
	regs.resolve(val)
}

/// Where a phi's incoming value comes from on a particular edge.
enum CopySource {
	Reg(String),
	/// A non-zero immediate.
	Imm(i64),
	Zero,
}

/// The simultaneous register copies that realise `succ`'s phi nodes for the
/// edge coming from `pred`. Returned as assembly text to be placed *on that
/// edge* - never before a conditional branch that might not take it.
fn phi_moves_for_edge<'ctx>(
	pred: BasicBlock<'ctx>,
	succ: BasicBlock<'ctx>,
	registers: &mut RegAlloc<'ctx>,
) -> String {
	registers.scratch_reset();

	let mut copies: Vec<(String, CopySource)> = Vec::new();

	for instr in succ.get_instructions() {
		if instr.get_opcode() != InstructionOpcode::Phi {
			break; // phis are contiguous at the top of a block
		}
		let phi: inkwell::values::PhiValue = instr.try_into().unwrap();
		let dest = registers.resolve(instr.as_any_value_enum().try_into().unwrap());

		for i in 0..phi.count_incoming() {
			let Some((incoming_val, incoming_block)) = phi.get_incoming(i) else { continue; };
			if incoming_block != pred {
				continue;
			}

			// An undef incoming value supplies nothing defined for this phi
			// (e.g. a loop-local merged along the pre-header edge) - no move.
			if !is_undef(incoming_val) {
				let source = match as_constant_i64(incoming_val) {
					Some(0) => CopySource::Zero,
					Some(v) => CopySource::Imm(v),
					None => CopySource::Reg(registers.resolve(incoming_val)),
				};
				copies.push((dest, source));
			}
			break; // one incoming edge per (phi, predecessor)
		}
	}

	sequence_parallel_copy(copies, registers)
}

/// Lower a set of copies that logically happen at once into a correct
/// sequence of `mv`/`li`. Register cycles (`a<-b, b<-a`) are broken with a
/// scratch register.
fn sequence_parallel_copy(copies: Vec<(String, CopySource)>, registers: &mut RegAlloc) -> String {
	// Copies already satisfied (destination already holds the source) drop out.
	let mut pending: Vec<(String, CopySource)> = copies
		.into_iter()
		.filter(|(dest, src)| !matches!(src, CopySource::Reg(r) if r == dest))
		.collect();

	let mut out = String::new();

	while !pending.is_empty() {
		let src_regs: HashSet<String> = pending
			.iter()
			.filter_map(|(_, src)| match src {
				CopySource::Reg(r) => Some(r.clone()),
				_ => None,
			})
			.collect();

		// A copy whose destination nobody else still needs as a source is
		// free to emit now.
		if let Some(pos) = pending.iter().position(|(dest, _)| !src_regs.contains(dest)) {
			let (dest, src) = pending.remove(pos);
			emit_copy(&mut out, &dest, &src);
			continue;
		}

		// Everything left is a register cycle. Park one destination's live
		// value in scratch and redirect whoever was reading it.
		let cycle_dest = pending[0].0.clone();
		let scratch = registers.next_scratch();
		out.push_str(&format!("\tmv\t{cycle_dest} {scratch}\n"));
		for (_, src) in pending.iter_mut() {
			if let CopySource::Reg(r) = src {
				if *r == cycle_dest {
					*r = scratch.clone();
				}
			}
		}
	}

	out
}

fn emit_copy(out: &mut String, dest: &str, src: &CopySource) {
	match src {
		CopySource::Reg(r) => out.push_str(&format!("\tmv\t{r} {dest}\n")),
		CopySource::Zero => out.push_str(&format!("\tmv\tzero {dest}\n")),
		CopySource::Imm(v) => out.push_str(&format!("\tli\t{v} {dest}\n")),
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

fn is_fused_into_next_br(icmp: InstructionValue) -> bool {
	matches!(
        icmp.get_next_instruction().map(|n| n.get_opcode()),
        Some(InstructionOpcode::Br)
    )
}

fn materialize_icmp<'ctx>(icmp: InstructionValue<'ctx>, registers: &mut RegAlloc<'ctx>) -> String {
	let lhs = icmp.get_operand(0).unwrap().unwrap_value();
	let rhs = icmp.get_operand(1).unwrap().unwrap_value();
	let predicate = icmp.get_icmp_predicate().unwrap();

	let mut out = String::new();
	let lhs_reg = resolve_operand(registers, &mut out, lhs);
	let rhs_reg = resolve_operand(registers, &mut out, rhs);
	let dest = registers.resolve(icmp.as_any_value_enum().try_into().unwrap());

	// unique-ish labels; instruction address works fine as a disambiguator
	let id = icmp.as_any_value_enum().into_int_value().as_value_ref() as usize;
	let true_label = format!("icmp_true_{id}");
	let end_label = format!("icmp_end_{id}");

	let mnemonic = branch_mnemonic(predicate);
	out.push_str(&format!("\tli\t0 {dest}\n"));
	out.push_str(&format!("\t{mnemonic}\t{lhs_reg} {rhs_reg} {true_label}\n"));
	out.push_str(&format!("\tj\t{end_label}\n"));
	out.push_str(&format!("{true_label}:\n"));
	out.push_str(&format!("\tli\t1 {dest}\n"));
	out.push_str(&format!("{end_label}:\n"));
	out
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
#[cfg(test)]
mod tests {
	use super::build;
	use crate::compiler::codegen::{BuildMode, Codegen};
	use crate::compiler::lexer::tokenize;
	use crate::compiler::parser::Parser;
	use crate::compiler::typecheck::check_program;
	use inkwell::context::Context;

	/// Compile a source file the same way `compile_to_asm` does and return the
	/// generated assembly.
	fn asm_for(file: &str) -> String {
		let source = std::fs::read_to_string(format!("./src/compiler/programs/{file}.ecc")).unwrap();
		let tokens = tokenize(&source);
		let mut program = Parser::new(tokens).parse_program();
		check_program(&mut program);

		let ctx = Context::create();
		let mut codegen = Codegen::new(&ctx, file, BuildMode::Release);
		codegen.lower_program(&program);
		crate::compiler::optimize(&codegen.module, "default<Oz>");
		codegen.module.verify().expect("module verifies");

		build(&codegen.module)
	}

	/// Every operand of every emitted instruction must be a real machine
	/// register, `zero`, or an immediate - never a leftover SSA name or an
	/// empty string, and the self-move peephole must have run.
	fn assert_well_formed(asm: &str) {
		for line in asm.lines() {
			let line = line.trim();
			if line.is_empty() || line.ends_with(':') {
				continue;
			}
			let mut parts = line.split_whitespace();
			let mnemonic = parts.next().unwrap();

			if mnemonic == "mv" {
				let regs: Vec<&str> = parts.collect();
				assert_eq!(regs.len(), 2, "mv takes two operands: {line}");
				assert_ne!(regs[0], regs[1], "self-move survived the peephole: {line}");
			}

			for tok in line.split_whitespace().skip(1) {
				assert!(!tok.contains('%'), "un-lowered SSA operand in `{line}`");
			}
		}
	}

	#[test]
	fn fib_lowers_and_realises_the_loop_carried_phi_cycle() {
		let asm = asm_for("fib");
		assert_well_formed(&asm);

		// The while-loop rotates b <- a, a <- addtmp through shared registers,
		// which is a true parallel-copy cycle: it must be broken with a scratch
		// register (t4/t5/t6), not lowered as a naive sequence of `mv`s.
		let body = asm
			.split("while.body")
			.nth(1)
			.expect("fib has a while.body block");
		let cycle_break = body
			.lines()
			.take_while(|l| !l.trim_start().starts_with('j'))
			.any(|l| {
				let l = l.trim();
				l.starts_with("mv\t") && (l.contains(" t4") || l.contains(" t5") || l.contains(" t6"))
			});
		assert!(cycle_break, "loop-carried phi cycle was not broken with scratch:\n{asm}");
	}

	#[test]
	fn malloc_places_phi_copies_on_their_edge() {
		let asm = asm_for("std_lib");
		assert_well_formed(&asm);

		// malloc's entry block ends in a conditional branch whose `then`
		// successor (common.ret) takes the constant 1 through a phi. That copy
		// must sit *after* the branch - on the edge - never hoisted above it
		// where the fall-through path to `if.end` would run it too.
		let malloc = asm.split("\nmalloc:").nth(1).expect("malloc emitted");
		let malloc = malloc.split("\nfree:").next().unwrap();

		let pre_branch: Vec<&str> = malloc
			.lines()
			.map(str::trim)
			.take_while(|l| {
				let m = l.split_whitespace().next().unwrap_or("");
				!(m == "j" || (m.starts_with('b') && m != "break"))
			})
			.collect();
		assert!(
			!pre_branch.iter().any(|l| l.starts_with("li\t1 ")),
			"phi constant was hoisted above the branch: {pre_branch:?}"
		);
		assert!(
			malloc.lines().any(|l| l.trim() == "li\t1 s0" || l.trim().starts_with("li\t1 ")),
			"expected the phi constant `1` to be materialised somewhere in malloc:\n{malloc}"
		);
	}
}
