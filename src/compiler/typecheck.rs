use crate::compiler::ast::*;
use std::collections::HashMap;

fn node_key(expr: &Expr) -> usize {
	expr as *const Expr as usize
}


#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TypeVar(usize);

pub struct SymbolTable {
	scopes: Vec<HashMap<String, Inferred>>,
}

impl SymbolTable {
	pub fn new() -> Self {
		SymbolTable { scopes: vec![HashMap::new()] }
	}

	pub fn push_scope(&mut self) {
		self.scopes.push(HashMap::new());
	}

	pub fn pop_scope(&mut self) {
		self.scopes.pop();
	}

	pub fn declare(&mut self, name: &str, ty: Inferred) {
		self.scopes.last_mut().unwrap().insert(name.to_string(), ty);
	}

	pub fn lookup(&self, name: &str) -> Option<Inferred> {
		for scope in self.scopes.iter().rev() {
			if let Some(ty) = scope.get(name) {
				return Some(ty.clone());
			}
		}
		None
	}
}

pub struct TypeContext {
	parents: Vec<usize>,
	resolved: Vec<Option<Type>>,
	/// Best-known source line + a short "what were we checking" phrase for the
	/// unification currently in progress. Set by [`unify_at`] so the deferred
	/// panics down here can point at real source.
	site: Option<(usize, String)>,
}

impl TypeContext {
	pub fn new() -> Self {
		TypeContext { parents: Vec::new(), resolved: Vec::new(), site: None }
	}

	fn where_str(&self) -> String {
		match &self.site {
			Some((line, what)) => format!(" (line {line}, {what})"),
			None => String::new(),
		}
	}

	pub fn fresh_var(&mut self) -> TypeVar {
		let id = self.parents.len();
		self.parents.push(id);
		self.resolved.push(None);
		TypeVar(id)
	}

	fn find(&mut self, var: TypeVar) -> usize {
		let mut id = var.0;
		while self.parents[id] != id {
			self.parents[id] = self.parents[self.parents[id]]; // path compression
			id = self.parents[id];
		}
		id
	}

	/// "this variable IS this concrete type" — e.g. from an explicit annotation
	pub fn unify_concrete(&mut self, var: TypeVar, ty: Type) {
		let root = self.find(var);
		match &self.resolved[root] {
			Some(existing) if *existing != ty =>
				panic!("type mismatch{}: this value is used as both {:?} and {:?}", self.where_str(), existing, ty),
			_ => self.resolved[root] = Some(ty),
		}
	}

	/// "these two variables must end up the same type" — e.g. both sides of a `+`
	pub fn unify_vars(&mut self, a: TypeVar, b: TypeVar) {
		let (ra, rb) = (self.find(a), self.find(b));
		if ra == rb { return; }
		let merged = match (&self.resolved[ra], &self.resolved[rb]) {
			(Some(t), None) | (None, Some(t)) => Some(t.clone()),
			(Some(t1), Some(t2)) if t1 == t2 => Some(t1.clone()),
			(Some(t1), Some(t2)) => panic!("type mismatch{}: {:?} vs {:?}", self.where_str(), t1, t2),
			(None, None) => None,
		};
		self.parents[rb] = ra;
		self.resolved[ra] = merged;
	}

	/// final answer, called only once you've walked everything — defaults if never pinned
	pub fn resolve(&mut self, var: TypeVar) -> Type {
		let root = self.find(var);
		self.resolved[root].clone().unwrap_or(Type::I16)
	}
}

#[derive(Debug, Clone)]
pub enum Inferred {
	Concrete(Type),
	Var(TypeVar),
}

/// name -> (param types, return type). `None` return type means void.
pub type FnSigs = HashMap<String, (Vec<Type>, Option<Type>)>;

fn build_fn_sigs(program: &Program) -> FnSigs {
	let mut sigs = FnSigs::new();
	for func in &program.functions {
		if let Stmt::FnDecl { name, params, return_ty, .. } = func {
			let param_types = params.iter().map(|p| p.ty.clone()).collect();
			sigs.insert(name.clone(), (param_types, return_ty.clone()));
		}
	}
	sigs
}

fn infer_expr(expr: &Expr, symbols: &SymbolTable, consts: &HashMap<String, (Type, i64)>, ctx: &mut TypeContext, scratch: &mut HashMap<usize, Inferred>, sigs: &FnSigs) -> Inferred {
	let result = match expr {
		Expr::IntLiteral(_) => Inferred::Var(ctx.fresh_var()),
		Expr::BoolLiteral(_) => Inferred::Concrete(Type::Bool),

		Expr::Identifier(name) => {
			if let Some(inf) = symbols.lookup(name) {
				inf
			} else if let Some((ty, _)) = consts.get(name) {
				Inferred::Concrete(ty.clone())
			} else {
				panic!("undefined variable '{name}'")
			}
		}

		Expr::Binary { op, left, right, line } => {
			let lt = infer_expr(left, symbols, consts, ctx, scratch, sigs);
			let rt = infer_expr(right, symbols, consts, ctx, scratch, sigs);
			unify_at(&lt, &rt, ctx, *line, || format!("operands of `{op:?}`"));
			match op {
				BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::Gt
				| BinOp::LtEq | BinOp::GtEq | BinOp::And | BinOp::Or => Inferred::Concrete(Type::Bool),
				_ => lt,
			}
		}
		Expr::Assign { target, value, line } => {
			let target_ty = infer_expr(target, symbols, consts, ctx, scratch, sigs);
			let value_ty = infer_expr(value, symbols, consts, ctx, scratch, sigs);
			unify_at(&target_ty, &value_ty, ctx, *line, || "assignment".to_string());
			target_ty
		}
		Expr::Unary { op, expr } => {
			let t = infer_expr(expr, symbols, consts, ctx, scratch, sigs);
			match op {
				UnOp::Neg => t,
				UnOp::Not => Inferred::Concrete(Type::Bool),
			}
		}
		Expr::Deref(inner) => {
			match infer_expr(inner, symbols, consts, ctx, scratch, sigs) {
				Inferred::Concrete(Type::Pointer { inner, .. }) => Inferred::Concrete(*inner),
				Inferred::Concrete(other) => panic!("cannot dereference non-pointer type {:?}", other),
				Inferred::Var(_) => panic!("cannot dereference a value of unknown type — annotate it"),
			}
		}
		Expr::Cast { ty, .. } => Inferred::Concrete(ty.clone()),
		Expr::AddrOf(inner) => {
			let inner_ty = infer_expr(inner, symbols, consts, ctx, scratch, sigs);
			match inner_ty {
				Inferred::Concrete(t) => Inferred::Concrete(Type::Pointer { mutable: true, inner: Box::new(t) }),
				Inferred::Var(_) => panic!("cannot take address of a value of unknown type — annotate it"),
			}
		}

		// =========================================================================================
		// `fetch` is the one true intrinsic - it's the primitive everything else
		// (including malloc/free, once you write them) bottoms out on, so it can't
		// itself be written in your language. Always reads one full Word.
		Expr::Call { name, args, .. } if name == "fetch" => {
			assert_eq!(args.len(), 1, "fetch(addr) takes exactly one argument");
			infer_expr(&args[0], symbols, consts, ctx, scratch, sigs);
			Inferred::Concrete(Type::I16)
		}

		Expr::Call { name, args, .. } if name == "store" => {
			assert_eq!(args.len(), 2, "store(addr, value) takes exactly two arguments");
			infer_expr(&args[0], symbols, consts, ctx, scratch, sigs);
			infer_expr(&args[1], symbols, consts, ctx, scratch, sigs);
			Inferred::Concrete(Type::I16)
		}

		// `port_out(port, value)` / `port_in(port)` - the IO-port intrinsics.
		// They lower straight to the custom ISA's `out` / `in` instructions, so
		// like `fetch` they can't be written in the language. `port` must be a
		// compile-time constant (it encodes as a 4-bit immediate); that's
		// enforced in codegen, not here.
		Expr::Call { name, args, .. } if name == "port_out" => {
			assert_eq!(args.len(), 2, "port_out(port, value) takes exactly two arguments");
			infer_expr(&args[0], symbols, consts, ctx, scratch, sigs);
			infer_expr(&args[1], symbols, consts, ctx, scratch, sigs);
			Inferred::Concrete(Type::I16)
		}

		Expr::Call { name, args, .. } if name == "port_in" => {
			assert_eq!(args.len(), 1, "port_in(port) takes exactly one argument");
			infer_expr(&args[0], symbols, consts, ctx, scratch, sigs);
			Inferred::Concrete(Type::I16)
		}

		// General function calls - looks up the real declared signature (this is
		// what malloc/free will go through once you write them as real `fn`s).
		Expr::Call { name, args, line } => {
			let (param_tys, return_ty) = sigs.get(name)
				.unwrap_or_else(|| panic!("call to undefined function '{name}' (line {line})"));

			if args.len() != param_tys.len() {
				panic!(
					"line {line}: `{name}` expects {} argument(s), got {}",
					param_tys.len(), args.len()
				);
			}

			for (i, (arg, expected)) in args.iter().zip(param_tys.iter()).enumerate() {
				let arg_ty = infer_expr(arg, symbols, consts, ctx, scratch, sigs);
				let (name, expected) = (name.clone(), expected.clone());
				unify_at(&arg_ty, &Inferred::Concrete(expected), ctx, *line, || {
					format!("argument {} of `{name}`", i + 1)
				});
			}

			match return_ty {
				Some(t) => Inferred::Concrete(t.clone()),
				// Same "not really modeled" gap as before for void - calling a
				// void function as a value-producing expression will silently
				// type as I16 rather than erroring. Fine for statement-position
				// calls (`free(p);`)
				None => Inferred::Concrete(Type::I16),
			}
		}
	};

	scratch.insert(node_key(expr), result.clone());
	result
}

fn unify(a: &Inferred, b: &Inferred, ctx: &mut TypeContext) {
	match (a, b) {
		(Inferred::Var(v1), Inferred::Var(v2)) => ctx.unify_vars(*v1, *v2),
		(Inferred::Var(v), Inferred::Concrete(t)) | (Inferred::Concrete(t), Inferred::Var(v)) =>
			ctx.unify_concrete(*v, t.clone()),
		(Inferred::Concrete(t1), Inferred::Concrete(t2)) if t1 != t2 =>
			panic!("type mismatch{}: {:?} vs {:?}", ctx.where_str(), t1, t2),
		_ => {}
	}
}

/// `unify`, but records where in the source we are first so every mismatch
/// panic (here and the deferred ones inside `TypeContext`) can name a line.
/// `what` is a short phrase like `"assignment"` or `` "argument 2 of `foo`" ``.
fn unify_at(
	a: &Inferred,
	b: &Inferred,
	ctx: &mut TypeContext,
	line: usize,
	what: impl FnOnce() -> String,
) {
	ctx.site = Some((line, what()));
	unify(a, b, ctx);
	ctx.site = None;
}

/// [`unify_at`] when the line is only maybe-known (statement-level checks
/// dig it out of a sub-expression, which doesn't always carry one).
fn unify_near(
	a: &Inferred,
	b: &Inferred,
	ctx: &mut TypeContext,
	line: Option<usize>,
	what: impl FnOnce() -> String,
) {
	match line {
		Some(l) => unify_at(a, b, ctx, l, what),
		None => unify(a, b, ctx),
	}
}

/// Best-effort source line for an expression: the operator/`=`/call sites
/// carry one directly; otherwise peer through the transparent wrappers.
fn expr_line(expr: &Expr) -> Option<usize> {
	match expr {
		Expr::Binary { line, .. } | Expr::Assign { line, .. } | Expr::Call { line, .. } => Some(*line),
		Expr::Unary { expr, .. } | Expr::Cast { expr, .. } | Expr::Deref(expr) | Expr::AddrOf(expr) => {
			expr_line(expr)
		}
		_ => None,
	}
}

fn stmt_key(stmt: &Stmt) -> usize {
	stmt as *const Stmt as usize
}

fn check_stmt(
	stmt: &Stmt,
	symbols: &mut SymbolTable,
	consts: &HashMap<String, (Type, i64)>,
	ctx: &mut TypeContext,
	scratch: &mut HashMap<usize, Inferred>,
	let_types: &mut HashMap<usize, Inferred>,
	sigs: &FnSigs,
) {
	match stmt {
		Stmt::Let { name, ty, value, .. } => {
			let value_ty = infer_expr(value, symbols, consts, ctx, scratch, sigs);
			let declared = match ty {
				Some(explicit) => {
					unify_near(&value_ty, &Inferred::Concrete(explicit.clone()), ctx,
						expr_line(value), || format!("`let {name}` initializer vs its `: {explicit:?}`"));
					Inferred::Concrete(explicit.clone())
				}
				None => value_ty,
			};
			symbols.declare(name, declared.clone());
			let_types.insert(stmt_key(stmt), declared); // remember it for write-back
		}

		Stmt::If { cond, then_branch, else_branch } => {
			let cond_ty = infer_expr(cond, symbols, consts, ctx, scratch, sigs);
			unify_near(&cond_ty, &Inferred::Concrete(Type::Bool), ctx,
				expr_line(cond), || "`if` condition".to_string());
			symbols.push_scope();
			for s in then_branch { check_stmt(s, symbols, consts, ctx, scratch, let_types, sigs); }
			symbols.pop_scope();
			if let Some(else_b) = else_branch {
				symbols.push_scope();
				for s in else_b { check_stmt(s, symbols, consts, ctx, scratch, let_types, sigs); }
				symbols.pop_scope();
			}
		}

		Stmt::While { cond, body } => {
			let cond_ty = infer_expr(cond, symbols, consts, ctx, scratch, sigs);
			unify_near(&cond_ty, &Inferred::Concrete(Type::Bool), ctx,
				expr_line(cond), || "`while` condition".to_string());
			symbols.push_scope();
			for s in body { check_stmt(s, symbols, consts, ctx, scratch, let_types, sigs); }
			symbols.pop_scope();
		}

		Stmt::Return(expr) => {
			if let Some(e) = expr { infer_expr(e, symbols, consts, ctx, scratch, sigs); }
		}

		Stmt::ExprStmt(expr) => { infer_expr(expr, symbols, consts, ctx, scratch, sigs); }
		Stmt::Continue => {} // no expression, nothing to infer
		Stmt::Break => {}
		Stmt::FnDecl { .. } => panic!("nested functions not supported yet"),
	}
}

/// second, mutable pass — ONLY visits Stmt::Let, writes the resolved
/// type into the field that was already there.
fn assign_let_types(stmt: &mut Stmt, resolved: &HashMap<usize, Type>) {
	let key = stmt_key(stmt);
	match stmt {
		Stmt::Let { ty, .. } => {
			if ty.is_none() {
				*ty = resolved.get(&key).cloned();
			}
		}
		Stmt::If { then_branch, else_branch, .. } => {
			for s in then_branch { assign_let_types(s, resolved); }
			if let Some(e) = else_branch {
				for s in e { assign_let_types(s, resolved); }
			}
		}
		Stmt::While { body, .. } => {
			for s in body { assign_let_types(s, resolved); }
		}
		_ => {}
	}
}

pub fn check_program(program: &mut Program) {
	let sigs = build_fn_sigs(program);

	for func in &mut program.functions {
		if let Stmt::FnDecl { params, body, .. } = func {
			let mut symbols = SymbolTable::new();
			let mut ctx = TypeContext::new();
			let mut scratch: HashMap<usize, Inferred> = HashMap::new();
			let mut let_types: HashMap<usize, Inferred> = HashMap::new();
			let consts: HashMap<String, (Type, i64)> = program.consts.iter()
				.map(|c| (c.name.clone(), (c.ty.clone(), c.value)))
				.collect();

			for param in params.iter() {
				symbols.declare(&param.name, Inferred::Concrete(param.ty.clone()));
			}

			for stmt in body.iter() {
				check_stmt(stmt, &mut symbols, &consts, &mut ctx, &mut scratch, &mut let_types, &sigs);
			}

			let resolved: HashMap<usize, Type> = let_types.into_iter()
				.map(|(k, inferred)| {
					let ty = match inferred {
						Inferred::Concrete(t) => t,
						Inferred::Var(v) => ctx.resolve(v),
					};
					(k, ty)
				})
				.collect();

			for stmt in body.iter_mut() {
				assign_let_types(stmt, &resolved);
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use crate::compiler::lexer::tokenize;
	use crate::compiler::parser::Parser;

	/// Type-check `src`, returning the panic message if it fails.
	fn check_err(src: &str) -> Option<String> {
		let mut prog = Parser::new(tokenize(src)).parse_program();
		std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| super::check_program(&mut prog)))
			.err()
			.map(|e| {
				e.downcast_ref::<String>().cloned()
					.or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
					.unwrap_or_default()
			})
	}

	#[test]
	fn type_errors_carry_a_source_line() {
		// call argument
		let msg = check_err(
			"fn wide(v: i32) -> i32 { return v; }\n\
			 fn main() {\n\
			 \tlet a: i16 = 1;\n\
			 \twide(a);\n\
			 }",
		)
		.expect("should not type-check");
		assert!(msg.contains("line 4"), "want line 4, got: {msg}");
		assert!(msg.contains("wide"), "want the callee name, got: {msg}");

		// binary operands
		let msg = check_err(
			"fn main() {\n\
			 \tlet a: i16 = 1;\n\
			 \tlet b: i32 = 2;\n\
			 \tlet c = a + b;\n\
			 }",
		)
		.expect("should not type-check");
		assert!(msg.contains("line 4"), "want line 4, got: {msg}");
	}

	#[test]
	fn well_typed_program_checks() {
		assert!(check_err(
			"fn main() {\n\
			 \tlet a: i16 = 3;\n\
			 \tlet b = a * 2 + 1;\n\
			 \tstore(0, b);\n\
			 }"
		)
		.is_none());
	}
}
