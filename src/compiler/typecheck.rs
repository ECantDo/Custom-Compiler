use crate::compiler::ast::*;
use std::collections::HashMap;

pub type TypeMap = HashMap<usize, Type>;

fn node_key(expr: &Expr) -> usize {
	expr as *const Expr as usize
}

pub fn type_of<'a>(map: &'a TypeMap, expr: &Expr) -> Option<&'a Type> {
	map.get(&node_key(expr))
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
}

impl TypeContext {
	pub fn new() -> Self {
		TypeContext { parents: Vec::new(), resolved: Vec::new() }
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
				panic!("type mismatch: inferred both {:?} and {:?}", existing, ty),
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
			(Some(t1), Some(t2)) => panic!("type mismatch: {:?} vs {:?}", t1, t2),
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
enum Inferred {
	Concrete(Type),
	Var(TypeVar),
}
fn infer_expr(expr: &Expr, symbols: &SymbolTable, ctx: &mut TypeContext, scratch: &mut HashMap<usize, Inferred>) -> Inferred {
	let result = match expr {
		Expr::IntLiteral(_) => Inferred::Var(ctx.fresh_var()),
		Expr::BoolLiteral(_) => Inferred::Concrete(Type::Bool),

		Expr::Identifier(name) => symbols.lookup(name)
			.unwrap_or_else(|| panic!("undefined variable '{name}'")),

		Expr::Binary { op, left, right } => {
			let lt = infer_expr(left, symbols, ctx, scratch);
			let rt = infer_expr(right, symbols, ctx, scratch);
			unify(&lt, &rt, ctx);
			match op {
				BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::Gt
				| BinOp::LtEq | BinOp::GtEq | BinOp::And | BinOp::Or => Inferred::Concrete(Type::Bool),
				_ => lt,
			}
		}
		Expr::Assign { target, value } => {
			let target_ty = infer_expr(target, symbols, ctx, scratch);
			let value_ty = infer_expr(value, symbols, ctx, scratch);
			unify(&target_ty, &value_ty, ctx);
			target_ty
		}
		Expr::Unary { op, expr } => {
			let t = infer_expr(expr, symbols, ctx, scratch);
			match op {
				UnOp::Neg => t,
				UnOp::Not => Inferred::Concrete(Type::Bool),
			}
		}
		Expr::Deref(inner) => {
			match infer_expr(inner, symbols, ctx, scratch) {
				Inferred::Concrete(Type::Pointer { inner, .. }) => Inferred::Concrete(*inner),
				Inferred::Concrete(other) => panic!("cannot dereference non-pointer type {:?}", other),
				Inferred::Var(_) => panic!("cannot dereference a value of unknown type — annotate it"),
			}
		}
		Expr::Cast { ty, .. } => Inferred::Concrete(ty.clone()),
		Expr::AddrOf(inner) => {
			let inner_ty = infer_expr(inner, symbols, ctx, scratch);
			match inner_ty {
				Inferred::Concrete(t) => Inferred::Concrete(Type::Pointer { mutable: true, inner: Box::new(t) }),
				Inferred::Var(_) => panic!("cannot take address of a value of unknown type — annotate it"),
			}
		}

		// =========================================================================================
		// Built-in Calls

		Expr::Call { name, args } if name == "malloc" => {
			for arg in args {
				infer_expr(arg, symbols, ctx, scratch);
			}
			Inferred::Concrete(Type::Pointer { mutable: true, inner: Box::new(Type::U16) })
		}
		Expr::Call { name, args } if name == "free" => {
			for arg in args {
				infer_expr(arg, symbols, ctx, scratch);
			}
			Inferred::Concrete(Type::I16) // Dummy return ... should be Void
		}
		Expr::Call {name, args} if name == "fetch" => {
			for arg in args {
				infer_expr(arg, symbols, ctx, scratch);
			}
			Inferred::Concrete(Type::I16) // actually returns something
		}

		Expr::Call { .. } => panic!("function calls not yet supported by the type checker"),

		_ => todo!(),
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
			panic!("type mismatch: {:?} vs {:?}", t1, t2),
		_ => {}
	}
}
fn check_let(name: &str, ty: &Option<Type>, value: &Expr, symbols: &mut SymbolTable, ctx: &mut TypeContext, scratch: &mut HashMap<usize, Inferred>) {
	let value_ty = infer_expr(value, symbols, ctx, scratch);

	let declared = match ty {
		Some(explicit) => {
			unify(&value_ty, &Inferred::Concrete(explicit.clone()), ctx);
			Inferred::Concrete(explicit.clone())
		}
		None => value_ty,
	};

	symbols.declare(name, declared);
}
fn stmt_key(stmt: &Stmt) -> usize {
	stmt as *const Stmt as usize
}

fn check_stmt(
	stmt: &Stmt,
	symbols: &mut SymbolTable,
	ctx: &mut TypeContext,
	scratch: &mut HashMap<usize, Inferred>,
	let_types: &mut HashMap<usize, Inferred>,
) {
	match stmt {
		Stmt::Let { name, ty, value, .. } => {
			let value_ty = infer_expr(value, symbols, ctx, scratch);
			let declared = match ty {
				Some(explicit) => {
					unify(&value_ty, &Inferred::Concrete(explicit.clone()), ctx);
					Inferred::Concrete(explicit.clone())
				}
				None => value_ty,
			};
			symbols.declare(name, declared.clone());
			let_types.insert(stmt_key(stmt), declared); // remember it for write-back
		}

		Stmt::If { cond, then_branch, else_branch } => {
			let cond_ty = infer_expr(cond, symbols, ctx, scratch);
			unify(&cond_ty, &Inferred::Concrete(Type::Bool), ctx);
			symbols.push_scope();
			for s in then_branch { check_stmt(s, symbols, ctx, scratch, let_types); }
			symbols.pop_scope();
			if let Some(else_b) = else_branch {
				symbols.push_scope();
				for s in else_b { check_stmt(s, symbols, ctx, scratch, let_types); }
				symbols.pop_scope();
			}
		}

		Stmt::While { cond, body } => {
			let cond_ty = infer_expr(cond, symbols, ctx, scratch);
			unify(&cond_ty, &Inferred::Concrete(Type::Bool), ctx);
			symbols.push_scope();
			for s in body { check_stmt(s, symbols, ctx, scratch, let_types); }
			symbols.pop_scope();
		}

		Stmt::Return(expr) => {
			if let Some(e) = expr { infer_expr(e, symbols, ctx, scratch); }
		}

		Stmt::ExprStmt(expr) => { infer_expr(expr, symbols, ctx, scratch); }

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
	for func in &mut program.functions {
		if let Stmt::FnDecl { params, body, .. } = func {
			let mut symbols = SymbolTable::new();
			let mut ctx = TypeContext::new();
			let mut scratch: HashMap<usize, Inferred> = HashMap::new();
			let mut let_types: HashMap<usize, Inferred> = HashMap::new();

			for param in params.iter() {
				symbols.declare(&param.name, Inferred::Concrete(param.ty.clone()));
			}

			for stmt in body.iter() {
				check_stmt(stmt, &mut symbols, &mut ctx, &mut scratch, &mut let_types);
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