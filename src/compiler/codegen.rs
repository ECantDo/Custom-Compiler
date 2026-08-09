use crate::compiler::ast::*;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};
use inkwell::IntPredicate;
use std::collections::HashMap;

pub struct Codegen<'ctx> {
	context: &'ctx Context,
	pub module: Module<'ctx>,
	builder: Builder<'ctx>,
}

struct FnScope<'ctx> {
	vars: HashMap<String, (PointerValue<'ctx>, Type)>,
}

impl<'ctx> Codegen<'ctx> {
	pub fn new(context: &'ctx Context, module_name: &str) -> Self {
		Codegen {
			context,
			module: context.create_module(module_name),
			builder: context.create_builder(),
		}
	}

	fn llvm_int_type(&self, ty: &Type) -> inkwell::types::IntType<'ctx> {
		match ty {
			Type::I16 | Type::U16 => self.context.i16_type(),
			Type::I32 | Type::U32 => self.context.i32_type(),
			Type::Bool => self.context.bool_type(),
			Type::Pointer { .. } => panic!("pointers not wired into codegen yet"),
		}
	}

	fn is_signed(ty: &Type) -> bool {
		matches!(ty, Type::I16 | Type::I32)
	}

	pub fn lower_program(&mut self, program: &Program) {
		// pass 1: declare every function's signature first, so calls/recursion
		// can find each other regardless of source order
		for func in &program.functions {
			if let Stmt::FnDecl { name, params, return_ty, .. } = func {
				let param_types: Vec<_> = params.iter()
					.map(|p| self.llvm_int_type(&p.ty).into())
					.collect();
				let fn_type = match return_ty {
					Some(rt) => self.llvm_int_type(rt).fn_type(&param_types, false),
					None => self.context.void_type().fn_type(&param_types, false),
				};
				self.module.add_function(name, fn_type, None);
			}
		}
		// pass 2: lower each body
		for func in &program.functions {
			self.lower_fn_decl(func);
		}
	}

	fn lower_fn_decl(&mut self, stmt: &Stmt) {
		let Stmt::FnDecl { name, params, body, .. } = stmt else { return; };

		let function = self.module.get_function(name).expect("pre-declared in pass 1");
		let entry = self.context.append_basic_block(function, "entry");
		self.builder.position_at_end(entry);

		let mut scope = FnScope { vars: HashMap::new() };

		// params: alloca + store, same as any other local
		for (i, param) in params.iter().enumerate() {
			let llvm_ty = self.llvm_int_type(&param.ty);
			let alloca = self.builder.build_alloca(llvm_ty, &param.name).unwrap();
			self.builder.build_store(alloca, function.get_nth_param(i as u32).unwrap()).unwrap();
			scope.vars.insert(param.name.clone(), (alloca, param.ty.clone()));
		}

		// hoist every `let` in this function (including inside if/while) into
		// entry-block allocas BEFORE lowering any statements — this is the fix
		// for the loop-alloca problem described above
		for (name, ty) in collect_lets(body) {
			let llvm_ty = self.llvm_int_type(&ty);
			let alloca = self.builder.build_alloca(llvm_ty, name).unwrap();
			scope.vars.insert(name.to_string(), (alloca, ty));
		}
		self.builder.position_at_end(entry); // allocas may have moved the cursor

		for s in body {
			self.lower_stmt(s, &mut scope, function);
		}

		if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
			self.builder.build_return(None).unwrap();
		}
	}

	fn lower_stmt(&mut self, stmt: &Stmt, scope: &mut FnScope<'ctx>, function: FunctionValue<'ctx>) {
		match stmt {
			Stmt::Let { name, value, .. } => {
				let (ptr, ty) = scope.vars.get(name).expect("hoisted above").clone();
				let (val, _) = self.lower_expr(value, Some(&ty), scope);
				self.builder.build_store(ptr, val).unwrap();
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
				for s in body { self.lower_stmt(s, scope, function); }
				if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
					self.builder.build_unconditional_branch(cond_bb).unwrap();
				}

				self.builder.position_at_end(end_bb);
			}

			Stmt::If { cond, then_branch, else_branch } => {
				let then_bb = self.context.append_basic_block(function, "if.then");
				let else_bb = self.context.append_basic_block(function, "if.else");
				let end_bb = self.context.append_basic_block(function, "if.end");

				let (cond_val, _) = self.lower_expr(cond, Some(&Type::Bool), scope);
				self.builder.build_conditional_branch(cond_val.into_int_value(), then_bb, else_bb).unwrap();

				self.builder.position_at_end(then_bb);
				for s in then_branch { self.lower_stmt(s, scope, function); }
				if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
					self.builder.build_unconditional_branch(end_bb).unwrap();
				}

				self.builder.position_at_end(else_bb);
				if let Some(else_b) = else_branch {
					for s in else_b { self.lower_stmt(s, scope, function); }
				}
				if self.builder.get_insert_block().unwrap().get_terminator().is_none() {
					self.builder.build_unconditional_branch(end_bb).unwrap();
				}

				self.builder.position_at_end(end_bb);
			}

			Stmt::FnDecl { .. } => panic!("nested functions not supported"),
		}
	}

	/// Returns the lowered value AND the Type it was lowered as — the caller
	/// (e.g. Binary) needs this to decide what an un-annotated literal sibling
	/// should adopt.
	fn lower_expr(&mut self, expr: &Expr, hint: Option<&Type>, scope: &mut FnScope<'ctx>) -> (BasicValueEnum<'ctx>, Type) {
		match expr {
			Expr::IntLiteral(n) => {
				let ty = hint.cloned().unwrap_or(Type::I16); // your default rule
				let llvm_ty = self.llvm_int_type(&ty);
				(llvm_ty.const_int(*n as u64, Self::is_signed(&ty)).into(), ty)
			}

			Expr::BoolLiteral(b) => {
				(self.context.bool_type().const_int(*b as u64, false).into(), Type::Bool)
			}

			Expr::Identifier(name) => {
				let (ptr, ty) = scope.vars.get(name).unwrap_or_else(|| panic!("undefined variable '{name}'")).clone();
				let llvm_ty = self.llvm_int_type(&ty);
				(self.builder.build_load(llvm_ty, ptr, name).unwrap(), ty)
			}

			Expr::Binary { op, left, right } => {
				let (lval, lty) = self.lower_expr(left, hint, scope);
				let (rval, _) = self.lower_expr(right, Some(&lty), scope);
				let (l, r) = (lval.into_int_value(), rval.into_int_value());
				let signed = Self::is_signed(&lty);

				match op {
					BinOp::Add => (self.builder.build_int_add(l, r, "addtmp").unwrap().into(), lty),
					BinOp::Sub => (self.builder.build_int_sub(l, r, "subtmp").unwrap().into(), lty),
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

			Expr::Assign { target, value } => {
				let name = match target.as_ref() {
					Expr::Identifier(name) => name,
					_ => panic!("only plain identifier assignment targets supported right now"),
				};
				let (ptr, ty) = scope.vars.get(name).unwrap().clone();
				let (val, _) = self.lower_expr(value, Some(&ty), scope);
				self.builder.build_store(ptr, val).unwrap();
				(val, ty)
			}

			Expr::Cast { expr, ty } => {
				let (val, from_ty) = self.lower_expr(expr, None, scope);
				let target_llvm = self.llvm_int_type(ty);
				let (from_bits, to_bits) = (self.llvm_int_type(&from_ty).get_bit_width(), target_llvm.get_bit_width());
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

			Expr::Call { .. } => panic!("function calls not wired into codegen yet"),
			Expr::Deref(_) | Expr::AddrOf(_) => panic!("pointers not wired into codegen yet"),
		}
	}
}

/// Recursively finds every `let` in a statement list, including inside
/// if/while bodies, so they can all be pre-allocated in the entry block.
/// NOTE: doesn't currently handle two `let`s with the SAME name in different
/// nested scopes (shadowing) — they'd collide on one alloca. Not an issue for
/// fib.ecc, but a real gap if you shadow a name later.
fn collect_lets(stmts: &[Stmt]) -> Vec<(&str, Type)> {
	let mut out = Vec::new();
	for stmt in stmts {
		match stmt {
			Stmt::Let { name, ty, .. } => out.push((name.as_str(), ty.clone().expect("typecheck should have filled this"))),
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