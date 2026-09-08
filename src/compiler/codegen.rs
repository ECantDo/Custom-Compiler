use crate::compiler::ast::*;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};
use inkwell::{AddressSpace, IntPredicate};
use std::collections::HashMap;

pub struct Codegen<'ctx> {
	context: &'ctx Context,
	pub module: Module<'ctx>,
	builder: Builder<'ctx>,
	// name -> (param types, return type). Populated in pass 1 of lower_program,
	// consumed by general function-call codegen. `None` return means void.
	fn_sigs: HashMap<String, (Vec<Type>, Option<Type>)>,
	build_mode: BuildMode,
	consts: HashMap<String, (Type, i64)>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum BuildMode { Dev, Release }

struct FnScope<'ctx> {
	allocas: HashMap<usize, (PointerValue<'ctx>, Type)>, // keyed by unique Stmt address, never collides
	names: Vec<HashMap<String, usize>>,                   // scope stack: name -> alloca id
	function: FunctionValue<'ctx>,
	loop_targets: Vec<(BasicBlock<'ctx>, BasicBlock<'ctx>)>,
}

impl<'ctx> FnScope<'ctx> {
	fn new(function: FunctionValue<'ctx>) -> Self {
		FnScope {
			allocas: HashMap::new(),
			names: vec![HashMap::new()],
			function,
			loop_targets: Vec::new(),
		}
	}

	fn push(&mut self) { self.names.push(HashMap::new()); }
	fn pop(&mut self) { self.names.pop(); }
	fn declare(&mut self, name: &str, id: usize) {
		self.names.last_mut().unwrap().insert(name.to_string(), id);
	}
	fn resolve(&self, name: &str) -> (PointerValue<'ctx>, Type) {
		for scope in self.names.iter().rev() {
			if let Some(id) = scope.get(name) {
				return self.allocas.get(id).unwrap().clone();
			}
		}
		panic!("undefined variable '{name}'");
	}

	fn try_resolve(&self, name: &str) -> Option<(PointerValue<'ctx>, Type)> {
		for scope in self.names.iter().rev() {
			if let Some(id) = scope.get(name) {
				return self.allocas.get(id).cloned();
			}
		}
		None
	}
}

fn stmt_key(stmt: &Stmt) -> usize { stmt as *const Stmt as usize }

impl<'ctx> Codegen<'ctx> {
	pub fn new(context: &'ctx Context, module_name: &str, build_mode: BuildMode) -> Self {
		Codegen {
			context,
			module: context.create_module(module_name),
			builder: context.create_builder(),
			fn_sigs: HashMap::new(),
			build_mode,
			consts: HashMap::default(),
		}
	}

	/// Strictly-integer type conversion. Used where we already know the type
	/// can't be a pointer (int literals, arithmetic operands, cast widths).
	fn llvm_int_type(&self, ty: &Type) -> inkwell::types::IntType<'ctx> {
		match ty {
			Type::I16 | Type::U16 => self.context.i16_type(),
			Type::I32 | Type::U32 => self.context.i32_type(),
			Type::Bool => self.context.bool_type(),
			Type::Pointer { .. } => panic!("expected an integer type, found a pointer type"),
		}
	}

	/// General type conversion covering pointers too. Used anywhere a value
	/// of ANY type might need an LLVM type (allocas, params, fn return types).
	fn llvm_basic_type(&self, ty: &Type) -> BasicTypeEnum<'ctx> {
		match ty {
			Type::Pointer { .. } => self.context.ptr_type(AddressSpace::default()).into(),
			_ => self.llvm_int_type(ty).into(),
		}
	}

	fn is_signed(ty: &Type) -> bool {
		matches!(ty, Type::I16 | Type::I32)
	}

	pub fn lower_program(&mut self, program: &Program) {
		for c in &program.consts {
			self.consts.insert(c.name.clone(), (c.ty.clone(), c.value));
		}
		// A module with a `main` is a program: asmgen has no calling
		// convention, so every other function must be inlined into `main` and
		// then dropped. A module without `main` is a library (e.g. the
		// stdlib) - leave its functions alone so they survive to be emitted.
		let is_program = program.functions.iter().any(|f| {
			matches!(f, Stmt::FnDecl { name, .. } if name == "main")
		});
		for func in &program.functions {
			if let Stmt::FnDecl { name, params, return_ty, .. } = func {
				let param_types: Vec<_> = params.iter()
					.map(|p| self.llvm_basic_type(&p.ty).into())
					.collect();
				let fn_type = match return_ty {
					Some(rt) => self.llvm_basic_type(rt).fn_type(&param_types, false),
					None => self.context.void_type().fn_type(&param_types, false),
				};
				let function = self.module.add_function(name, fn_type, None);
				mark_null_pointer_valid(self.context, function);
				if is_program && name != "main" {
					let kind = Attribute::get_named_enum_kind_id("alwaysinline");
					function.add_attribute(AttributeLoc::Function, self.context.create_enum_attribute(kind, 0));
					function.set_linkage(inkwell::module::Linkage::Internal);
				}

				let sig_param_types = params.iter().map(|p| p.ty.clone()).collect();
				self.fn_sigs.insert(name.clone(), (sig_param_types, return_ty.clone()));
			}
		}
		for func in &program.functions {
			self.lower_fn_decl(func);
		}
	}

	fn lower_fn_decl(&mut self, stmt: &Stmt) {
		let Stmt::FnDecl { name, params, body, .. } = stmt else { return; };

		let function = self.module.get_function(name).expect("pre-declared in pass 1");
		let entry = self.context.append_basic_block(function, "entry");
		self.builder.position_at_end(entry);

		let mut scope = FnScope::new(function);

		// params: alloca + store, each param gets a synthetic id since it has
		// no Stmt of its own — using the param's index is fine, guaranteed
		// unique within this function and never collides with a real Stmt address
		for (i, param) in params.iter().enumerate() {
			let llvm_ty = self.llvm_basic_type(&param.ty);
			let alloca = self.builder.build_alloca(llvm_ty, &param.name).unwrap();
			self.builder.build_store(alloca, function.get_nth_param(i as u32).unwrap()).unwrap();
			let id = i; // synthetic id, distinct namespace from Stmt addresses in practice
			scope.allocas.insert(id, (alloca, param.ty.clone()));
			scope.declare(&param.name, id);
		}

		// hoist every `let` (including inside if/while) into entry-block allocas
		for (id, name, ty) in collect_lets(body) {
			let llvm_ty = self.llvm_basic_type(&ty);
			let alloca = self.builder.build_alloca(llvm_ty, name).unwrap();
			scope.allocas.insert(id, (alloca, ty));
		}
		self.builder.position_at_end(entry);

		for s in body {
			self.lower_stmt(s, &mut scope, function);
		}

		if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
			self.builder.build_return(None).unwrap();
		}
	}

	/// Lowers a list of statements, stopping early if one of them terminates
	/// the current block (return/continue/break) — anything after that point
	/// is unreachable and must NOT be lowered, or LLVM will reject the IR.
	fn lower_stmts(&mut self, stmts: &[Stmt], scope: &mut FnScope<'ctx>, function: FunctionValue<'ctx>) {
		for s in stmts {
			self.lower_stmt(s, scope, function);
			if self.builder.get_insert_block().unwrap().get_terminator().is_some() {
				break;
			}
		}
	}

	fn lower_stmt(&mut self, stmt: &Stmt, scope: &mut FnScope<'ctx>, function: FunctionValue<'ctx>) {
		match stmt {
			Stmt::Let { name, value, .. } => {
				let id = stmt_key(stmt);
				let (ptr, ty) = scope.allocas.get(&id).expect("hoisted above").clone();
				let (val, _) = self.lower_expr(value, Some(&ty), scope);
				self.builder.build_store(ptr, val).unwrap();
				scope.declare(name, id); // name becomes visible from here onward
			}

			Stmt::ExprStmt(expr) => { self.lower_expr(expr, None, scope); }

			Stmt::Return(expr) => match expr {
				Some(e) => {
					let (val, _) = self.lower_expr(e, None, scope);
					self.builder.build_return(Some(&val)).unwrap();
				}
				None => { self.builder.build_return(None).unwrap(); }
			},

			Stmt::While { cond, body } => {
				let cond_bb = self.context.append_basic_block(function, "while.cond");
				let body_bb = self.context.append_basic_block(function, "while.body");
				let end_bb = self.context.append_basic_block(function, "while.end");

				self.builder.build_unconditional_branch(cond_bb).unwrap();
				self.builder.position_at_end(cond_bb);
				let (cond_val, _) = self.lower_expr(cond, Some(&Type::Bool), scope);
				self.builder.build_conditional_branch(cond_val.into_int_value(), body_bb, end_bb).unwrap();

				self.builder.position_at_end(body_bb);
				scope.push();
				scope.loop_targets.push((cond_bb, end_bb)); // <-- both pushed together
				self.lower_stmts(body, scope, function);
				scope.loop_targets.pop();
				scope.pop();
				if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
					self.builder.build_unconditional_branch(cond_bb).unwrap();
				}

				self.builder.position_at_end(end_bb);
			}
			Stmt::Continue => {
				let (target, _) = *scope.loop_targets.last()
					.unwrap_or_else(|| panic!("'continue' used outside of a loop"));
				self.builder.build_unconditional_branch(target).unwrap();
			}
			Stmt::Break => {
				let (_, target) = *scope.loop_targets.last()
					.unwrap_or_else(|| panic!("'break' used outside of a loop"));
				self.builder.build_unconditional_branch(target).unwrap();
			}

			Stmt::If { cond, then_branch, else_branch } => {
				let then_bb = self.context.append_basic_block(function, "if.then");
				let else_bb = self.context.append_basic_block(function, "if.else");
				let end_bb = self.context.append_basic_block(function, "if.end");

				let (cond_val, _) = self.lower_expr(cond, Some(&Type::Bool), scope);
				self.builder.build_conditional_branch(cond_val.into_int_value(), then_bb, else_bb).unwrap();

				self.builder.position_at_end(then_bb);
				scope.push();
				for s in then_branch { self.lower_stmt(s, scope, function); }
				scope.pop();
				if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
					self.builder.build_unconditional_branch(end_bb).unwrap();
				}

				self.builder.position_at_end(else_bb);
				if let Some(else_b) = else_branch {
					scope.push();
					for s in else_b { self.lower_stmt(s, scope, function); }
					scope.pop();
				}
				if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
					self.builder.build_unconditional_branch(end_bb).unwrap();
				}

				self.builder.position_at_end(end_bb);
			}

			Stmt::FnDecl { .. } => panic!("nested functions not supported"),
		}
	}

	fn lower_expr(&mut self, expr: &Expr, hint: Option<&Type>, scope: &mut FnScope<'ctx>) -> (BasicValueEnum<'ctx>, Type) {
		match expr {
			Expr::IntLiteral(n) => {
				let ty = hint.cloned().unwrap_or(Type::I16);
				match &ty {
					Type::Pointer { .. } if *n == 0 => {
						let ptr_ty = self.context.ptr_type(AddressSpace::default());
						(ptr_ty.const_null().into(), ty)
					}
					Type::Pointer { .. } => panic!(
						"only `0` is supported as a raw pointer literal right now (got {n}) - cast an integer instead"
					),
					_ => {
						let llvm_ty = self.llvm_int_type(&ty);
						(llvm_ty.const_int(*n as u64, Self::is_signed(&ty)).into(), ty)
					}
				}
			}

			Expr::BoolLiteral(b) => {
				(self.context.bool_type().const_int(*b as u64, false).into(), Type::Bool)
			}

			Expr::Identifier(name) => {
				if let Some((ptr, ty)) = scope.try_resolve(name) {
					let llvm_ty = self.llvm_basic_type(&ty);
					(self.builder.build_load(llvm_ty, ptr, name).unwrap(), ty)
				} else if let Some((ty, value)) = self.consts.get(name).cloned() {
					let llvm_ty = self.llvm_int_type(&ty);
					(llvm_ty.const_int(value as u64, Self::is_signed(&ty)).into(), ty)
				} else {
					panic!("undefined variable '{name}'")
				}
			}

			Expr::Binary { op, left, right, .. } => {
				let (lval, lty) = self.lower_expr(left, hint, scope);
				let (rval, _) = self.lower_expr(right, Some(&lty), scope);
				let (l, r) = (lval.into_int_value(), rval.into_int_value());
				let signed = Self::is_signed(&lty);

				match op {
					BinOp::Add => (self.builder.build_int_add(l, r, "addtmp").unwrap().into(), lty),
					BinOp::Sub => (self.builder.build_int_sub(l, r, "subtmp").unwrap().into(), lty),
					// The target has no multiply/divide unit - asmgen lowers
					// these to shift/add sequences (constants) or a shift loop
					// (variables). `-Oz` strength-reduces the constant cases
					// first, so most of what reaches asmgen is already shifts.
					BinOp::Mul => (self.builder.build_int_mul(l, r, "multmp").unwrap().into(), lty),
					BinOp::Div => {
						let q = if signed {
							self.builder.build_int_signed_div(l, r, "sdivtmp").unwrap()
						} else {
							self.builder.build_int_unsigned_div(l, r, "udivtmp").unwrap()
						};
						(q.into(), lty)
					}
					BinOp::Rem => {
						let m = if signed {
							self.builder.build_int_signed_rem(l, r, "sremtmp").unwrap()
						} else {
							self.builder.build_int_unsigned_rem(l, r, "uremtmp").unwrap()
						};
						(m.into(), lty)
					}
					BinOp::BitAnd => (self.builder.build_and(l, r, "andtmp").unwrap().into(), lty),
					BinOp::BitOr => (self.builder.build_or(l, r, "ortmp").unwrap().into(), lty),
					BinOp::BitXor => (self.builder.build_xor(l, r, "xortmp").unwrap().into(), lty),
					BinOp::Shl => (self.builder.build_left_shift(l, r, "shltmp").unwrap().into(), lty),
					BinOp::Shr => (self.builder.build_right_shift(l, r, signed, "shrtmp").unwrap().into(), lty),
					BinOp::And => (self.builder.build_and(l, r, "landtmp").unwrap().into(), Type::Bool),
					BinOp::Or => (self.builder.build_or(l, r, "lortmp").unwrap().into(), Type::Bool),
					BinOp::Eq => (self.builder.build_int_compare(IntPredicate::EQ, l, r, "eqtmp").unwrap().into(), Type::Bool),
					BinOp::NotEq => (self.builder.build_int_compare(IntPredicate::NE, l, r, "netmp").unwrap().into(), Type::Bool),
					BinOp::Lt => (self.builder.build_int_compare(if signed { IntPredicate::SLT } else { IntPredicate::ULT }, l, r, "lttmp").unwrap().into(), Type::Bool),
					BinOp::Gt => (self.builder.build_int_compare(if signed { IntPredicate::SGT } else { IntPredicate::UGT }, l, r, "gttmp").unwrap().into(), Type::Bool),
					BinOp::LtEq => (self.builder.build_int_compare(if signed { IntPredicate::SLE } else { IntPredicate::ULE }, l, r, "letmp").unwrap().into(), Type::Bool),
					BinOp::GtEq => (self.builder.build_int_compare(if signed { IntPredicate::SGE } else { IntPredicate::UGE }, l, r, "getmp").unwrap().into(), Type::Bool),
				}
			}

			Expr::Unary { op, expr } => {
				let (val, ty) = self.lower_expr(expr, hint, scope);
				match op {
					UnOp::Neg => (self.builder.build_int_neg(val.into_int_value(), "negtmp").unwrap().into(), ty),
					UnOp::Not => (self.builder.build_not(val.into_int_value(), "nottmp").unwrap().into(), Type::Bool),
				}
			}

			Expr::Assign { target, value, .. } => {
				match target.as_ref() {
					Expr::Identifier(name) => {
						let (ptr, ty) = scope.resolve(name);
						let (val, _) = self.lower_expr(value, Some(&ty), scope);
						self.builder.build_store(ptr, val).unwrap();
						(val, ty)
					}
					Expr::Deref(inner) => {
						let (ptr_val, ptr_ty) = self.lower_expr(inner, None, scope);
						let inner_ty = match ptr_ty {
							Type::Pointer { inner, .. } => *inner,
							other => panic!("cannot dereference non-pointer type {other:?} as an assignment target"),
						};
						let (val, _) = self.lower_expr(value, Some(&inner_ty), scope);
						self.builder.build_store(ptr_val.into_pointer_value(), val).unwrap();
						(val, inner_ty)
					}
					_ => panic!("only plain identifiers and *pointer derefs are supported as assignment targets right now"),
				}
			}

			Expr::Cast { expr, ty } => {
				let (val, from_ty) = self.lower_expr(expr, None, scope);

				match (&from_ty, ty) {
					// pointer-to-pointer: opaque pointers carry no LLVM-level
					// type info, so this is a relabel, not a real instruction.
					(Type::Pointer { .. }, Type::Pointer { .. }) => (val, ty.clone()),

					(Type::Pointer { .. }, _) => {
						let target = self.llvm_int_type(ty);
						let result = self.builder
							.build_ptr_to_int(val.into_pointer_value(), target, "ptrtointtmp")
							.unwrap();
						(result.into(), ty.clone())
					}

					(_, Type::Pointer { .. }) => {
						let ptr_ty = self.context.ptr_type(AddressSpace::default());
						let result = self.builder
							.build_int_to_ptr(val.into_int_value(), ptr_ty, "inttoptrtmp")
							.unwrap();
						(result.into(), ty.clone())
					}

					_ => {
						let target_llvm = self.llvm_int_type(ty);
						let (from_bits, to_bits) = (
							self.llvm_int_type(&from_ty).get_bit_width(),
							target_llvm.get_bit_width(),
						);
						let result = if from_bits < to_bits {
							if Self::is_signed(&from_ty) {
								self.builder.build_int_s_extend(val.into_int_value(), target_llvm, "sexttmp").unwrap()
							} else {
								self.builder.build_int_z_extend(val.into_int_value(), target_llvm, "zexttmp").unwrap()
							}
						} else if from_bits > to_bits {
							self.builder.build_int_truncate(val.into_int_value(), target_llvm, "trunctmp").unwrap()
						} else {
							val.into_int_value()
						};
						(result.into(), ty.clone())
					}
				}
			}

			// `fetch(addr)` is the one true intrinsic - lowers straight to an
			// int-to-pointer cast + load, no real function call involved.
			// Always reads a full Word; confirm that's the width you actually
			// want (matches `lod` from the CPU project, but worth double-checking
			// this compiler is meant to model that at all given it targets the
			// host's real architecture via LLVM, not your custom ISA).
			Expr::Call { name, args, .. } if name == "fetch" => {
				assert_eq!(args.len(), 1, "fetch(addr) takes exactly one argument");
				let (addr_val, _addr_ty) = self.lower_expr(&args[0], None, scope);
				self.check_zero_address_allowed(addr_val, scope);

				let word_ty = self.context.i16_type();
				let ptr_ty = self.context.ptr_type(AddressSpace::default());
				let ptr = self.builder
					.build_int_to_ptr(addr_val.into_int_value(), ptr_ty, "fetchaddr")
					.unwrap();
				let loaded = self.builder.build_load(word_ty, ptr, "fetchval").unwrap();
				(loaded, Type::I16)
			}

			Expr::Call { name, args, .. } if name == "store" => {
				assert_eq!(args.len(), 2, "store(addr, value) takes exactly two arguments");
				let (addr_val, _addr_ty) = self.lower_expr(&args[0], None, scope);
				self.check_zero_address_allowed(addr_val, scope);

				let (value_val, _) = self.lower_expr(&args[1], Some(&Type::I16), scope);

				let ptr_ty = self.context.ptr_type(AddressSpace::default());
				let ptr = self.builder
					.build_int_to_ptr(addr_val.into_int_value(), ptr_ty, "storeaddr")
					.unwrap();

				self.builder.build_store(ptr, value_val).unwrap();

				// store doesn't produce a meaningful value; same "dummy I16(0)" convention
				(self.context.i16_type().const_int(0, false).into(), Type::I16)
			}

			// `port_out(port, value)` / `port_in(port)` - the IO-port intrinsics.
			// Like `fetch`/`store` they can't be written in the language: they
			// lower straight to the custom ISA's `out` / `in` instructions
			// (opcode 8), which asmgen picks out by callee name. `port` has to
			// be a compile-time constant - it encodes as a 4-bit immediate.
			//
			// The declarations are left attribute-free on purpose: LLVM then
			// treats every call as a full memory barrier, so `Oz` won't drop,
			// merge, hoist or reorder them - exactly the semantics MMIO wants.
			Expr::Call { name, args, .. } if name == "port_out" => {
				assert_eq!(args.len(), 2, "port_out(port, value) takes exactly two arguments");
				let port = self.lower_const_port(&args[0], scope);
				let (value_val, _) = self.lower_expr(&args[1], Some(&Type::I16), scope);

				let i16_ty = self.context.i16_type();
				let fn_ty = self.context.void_type().fn_type(&[i16_ty.into(), i16_ty.into()], false);
				let callee = self.module.get_function(IO_OUT_FN)
					.unwrap_or_else(|| self.module.add_function(IO_OUT_FN, fn_ty, None));

				self.builder
					.build_call(callee, &[i16_ty.const_int(port as u64, true).into(), value_val.into()], "")
					.unwrap();

				(i16_ty.const_int(0, false).into(), Type::I16)
			}

			Expr::Call { name, args, .. } if name == "port_in" => {
				assert_eq!(args.len(), 1, "port_in(port) takes exactly one argument");
				let port = self.lower_const_port(&args[0], scope);

				let i16_ty = self.context.i16_type();
				let fn_ty = i16_ty.fn_type(&[i16_ty.into()], false);
				let callee = self.module.get_function(IO_IN_FN)
					.unwrap_or_else(|| self.module.add_function(IO_IN_FN, fn_ty, None));

				let call = self.builder
					.build_call(callee, &[i16_ty.const_int(port as u64, true).into()], "inval")
					.unwrap();

				(call.try_as_basic_value().unwrap_basic(), Type::I16)
			}

			// General function calls - this is the path malloc/free will go
			// through once you write them as real `fn`s in your language.
			Expr::Call { name, args, .. } => {
				let function = self.module.get_function(name)
					.unwrap_or_else(|| panic!("undefined function '{name}'"));
				let (param_tys, return_ty) = self.fn_sigs.get(name)
					.unwrap_or_else(|| panic!("no recorded signature for function '{name}'"))
					.clone();

				let arg_vals: Vec<_> = args.iter().zip(param_tys.iter())
					.map(|(arg, pty)| self.lower_expr(arg, Some(pty), scope).0.into())
					.collect();

				let call = self.builder.build_call(function, &arg_vals, "calltmp").unwrap();

				match return_ty {
					Some(ret_ty) => {
						let val = call.try_as_basic_value().unwrap_basic();
						//.expect("function has a declared return type but produced no value");
						(val, ret_ty)
					}
					// Void call. Fine in statement position (`draw_pixel(x, y);`);
					// hand back the same dummy `I16(0)` `store` uses so a stray
					// value-position use types as I16 rather than crashing here -
					// matching the "void isn't really modelled" gap in typecheck.
					None => (self.context.i16_type().const_int(0, false).into(), Type::I16),
				}
			}

			Expr::Deref(inner) => {
				let (ptr_val, ptr_ty) = self.lower_expr(inner, None, scope);
				let inner_ty = match ptr_ty {
					Type::Pointer { inner, .. } => *inner,
					other => panic!("cannot dereference non-pointer type {other:?}"),
				};
				let llvm_inner_ty = self.llvm_basic_type(&inner_ty);
				let loaded = self.builder
					.build_load(llvm_inner_ty, ptr_val.into_pointer_value(), "derefload")
					.unwrap();
				(loaded, inner_ty)
			}

			Expr::AddrOf(inner) => {
				match inner.as_ref() {
					Expr::Identifier(name) => {
						let (ptr, ty) = scope.resolve(name);
						// NOTE: always marked mutable - FnScope doesn't currently
						// track whether the underlying `let` was declared `mut`,
						// so this doesn't yet enforce const-pointer safety.
						(ptr.into(), Type::Pointer { mutable: true, inner: Box::new(ty) })
					}
					_ => panic!("can only take the address of a plain variable right now"),
				}
			}
		}
	}

	fn check_zero_address_allowed(&mut self, addr_val: BasicValueEnum<'ctx>, scope: &FnScope<'ctx>) {
		if self.build_mode != BuildMode::Dev { return; }

		let fn_name = scope.function.get_name().to_str().unwrap();
		if fn_name == "malloc" || fn_name == "free" || fn_name == "initialize_heap" { return; }

		let i16_type = self.context.i16_type();
		let is_zero = self.builder.build_int_compare(
			IntPredicate::EQ, addr_val.into_int_value(), i16_type.const_int(0, false), "is_zero_addr",
		).unwrap();

		let trap_bb = self.context.append_basic_block(scope.function, "zero_addr_trap");
		let cont_bb = self.context.append_basic_block(scope.function, "zero_addr_ok");
		self.builder.build_conditional_branch(is_zero, trap_bb, cont_bb).unwrap();

		self.builder.position_at_end(trap_bb);
		let trap_intrinsic = inkwell::intrinsics::Intrinsic::find("llvm.trap").unwrap();
		let trap_fn = trap_intrinsic.get_declaration(&self.module, &[]).unwrap();
		self.builder.build_call(trap_fn, &[], "").unwrap();
		self.builder.build_unreachable().unwrap();

		self.builder.position_at_end(cont_bb);
	}

	/// Lower a `port_in`/`port_out` port argument. The port becomes a 4-bit
	/// immediate in the emitted instruction, so it has to fold to a
	/// compile-time constant in `0..=15` (an integer literal, or a `const`).
	fn lower_const_port(&mut self, expr: &Expr, scope: &mut FnScope<'ctx>) -> i64 {
		let (val, _ty) = self.lower_expr(expr, Some(&Type::I16), scope);
		let port = val
			.into_int_value()
			.get_sign_extended_constant()
			.unwrap_or_else(|| panic!("IO port must be a compile-time constant (an integer literal or a `const`)"));
		assert!(
			(0..=15).contains(&port),
			"IO port {port} is out of range - the hardware only has ports 0..=15",
		);
		port
	}
}

/// Module-level names of the IO-port intrinsic declarations. asmgen keys off
/// these when selecting the `out` / `in` instructions.
pub const IO_OUT_FN: &str = "__io_out";
pub const IO_IN_FN: &str = "__io_in";

/// Recursively finds every `let` in a statement list, including inside
/// if/while bodies, so they can all be pre-allocated in the entry block.
/// Returns each one's unique Stmt address as an id, so two `let`s with the
/// SAME name in different (even sibling) scopes no longer collide.
fn collect_lets(stmts: &[Stmt]) -> Vec<(usize, &str, Type)> {
	let mut out = Vec::new();
	for stmt in stmts {
		match stmt {
			Stmt::Let { name, ty, .. } => out.push((
				stmt_key(stmt),
				name.as_str(),
				ty.clone().expect("typecheck should have filled this"),
			)),
			Stmt::If { then_branch, else_branch, .. } => {
				out.extend(collect_lets(then_branch));
				if let Some(e) = else_branch { out.extend(collect_lets(e)); }
			}
			Stmt::While { body, .. } => out.extend(collect_lets(body)),
			_ => {}
		}
	}
	out
}


fn mark_null_pointer_valid(context: &Context, function: FunctionValue) {
	let kind_id = Attribute::get_named_enum_kind_id("null_pointer_is_valid");
	let attr = context.create_enum_attribute(kind_id, 0);
	function.add_attribute(AttributeLoc::Function, attr);
}