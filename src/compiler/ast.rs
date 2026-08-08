//formatter:off

#[derive(Debug, Clone, PartialEq)]
pub enum Type {
	I16, I32, U16, U32,
	Pointer { mutable: bool, inner: Box<Type> }, // *mut T / *const T
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinOp {
	Add, Sub,
	BitAnd, BitOr, BitXor, Shl, Shr,
	Eq, NotEq, Lt, Gt, LtEq, GtEq,
	And, Or, // && ||
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum UnOp {
	Neg,  // -x
	Not,  // !x
}

#[derive(Debug, Clone)]
pub enum Expr {
	IntLiteral(i64),
	Identifier(String),
	Binary { op: BinOp, left: Box<Expr>, right: Box<Expr> },
	Unary { op: UnOp, expr: Box<Expr> },
	Assign { target: Box<Expr>, value: Box<Expr> },
	Call { name: String, args: Vec<Expr> },
	Deref(Box<Expr>),                     // *p
	AddrOf(Box<Expr>),                    // &x
	Cast { expr: Box<Expr>, ty: Type },   // x as i32
}

#[derive(Debug, Clone)]
pub struct Param {
	pub name: String,
	pub ty: Type,
}

#[derive(Debug, Clone)]
pub enum Stmt {
	Let { name: String, mutable: bool, ty: Option<Type>, value: Expr },
	If { cond: Expr, then_branch: Vec<Stmt>, else_branch: Option<Vec<Stmt>> },
	While { cond: Expr, body: Vec<Stmt> },
	Return(Option<Expr>),
	ExprStmt(Expr),
	FnDecl { name: String, params: Vec<Param>, return_ty: Option<Type>, body: Vec<Stmt> },
}