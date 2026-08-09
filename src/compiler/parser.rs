use crate::compiler::{ast::*, lexer::{Token, TokenKind}};
use std::collections::HashMap;

pub struct Parser {
	tokens: Vec<Token>,
	pos: usize,
	const_values: HashMap<String, i64>,
}

impl Parser {
	pub fn new(tokens: Vec<Token>) -> Self {
		Parser { tokens, pos: 0, const_values: HashMap::new() }
	}

	fn peek(&self) -> &TokenKind {
		&self.tokens[self.pos].kind
	}

	fn advance(&mut self) -> &Token {
		let tok = &self.tokens[self.pos];
		if self.pos < self.tokens.len() - 1 {
			self.pos += 1;
		}
		tok
	}

	fn check(&self, kind: &TokenKind) -> bool {
		self.peek() == kind
	}

	fn expect(&mut self, kind: TokenKind) -> &Token {
		if self.check(&kind) {
			self.advance()
		} else {
			panic!("expected {:?}, found {:?} at line {}", kind, self.peek(), self.tokens[self.pos].line);
		}
	}

	fn parse_expression(&mut self) -> Expr {
		let expr = self.parse_binary(2);
		if self.check(&TokenKind::Eq) {
			self.advance();
			let value = self.parse_expression();
			return Expr::Assign { target: Box::new(expr), value: Box::new(value) };
		}

		expr
	}

	fn parse_unary(&mut self) -> Expr {
		match self.peek() {
			TokenKind::Minus => {
				self.advance();
				Expr::Unary { op: UnOp::Neg, expr: Box::new(self.parse_unary()) }
			}
			TokenKind::Bang => {
				self.advance();
				Expr::Unary { op: UnOp::Not, expr: Box::new(self.parse_unary()) }
			}
			TokenKind::Star => {
				self.advance();
				Expr::Deref(Box::new(self.parse_unary()))
			}
			TokenKind::Amp => {
				self.advance();
				Expr::AddrOf(Box::new(self.parse_unary()))
			}
			_ => self.parse_cast()
		}
	}

	fn parse_cast(&mut self) -> Expr {
		let mut expr = self.parse_primary();
		while self.check(&TokenKind::As) { // consume `x as as as as i16`
			self.advance();
			let ty = self.parse_type();
			expr = Expr::Cast { expr: Box::new(expr), ty };
		}
		expr
	}

	fn parse_binary(&mut self, min_prec: u8) -> Expr {
		let mut left = self.parse_unary();
		loop {
			let (op, prec) = match binop_info(self.peek()) {
				Some(info) if info.1 >= min_prec => info,
				_ => break,
			};
			self.advance();
			let right = self.parse_binary(prec + 1);
			left = Expr::Binary { op, left: Box::new(left), right: Box::new(right) };
		}

		left
	}

	fn parse_type(&mut self) -> Type {
		if self.check(&TokenKind::Star) {
			let line = self.peek_line();
			self.advance();
			let mutable = if self.check(&TokenKind::Mut) {
				self.advance();
				true
			} else if self.check(&TokenKind::As) {
				panic!("const pointers not wired up yet at line {}", line);
			} else {
				panic!("expected 'mut' or 'const' after '*' in pointer type at line {}", line);
			};
			let inner = self.parse_type();
			return Type::Pointer { mutable, inner: Box::new(inner) };
		}

		let line = self.peek_line();
		match self.advance().kind.clone() {
			TokenKind::Identifier(name) => match name.as_str() {
				"i16" => Type::I16,
				"i32" => Type::I32,
				"u16" => Type::U16,
				"u32" => Type::U32,
				"bool" => Type::Bool,
				other => panic!("unknown type '{other}' at line {}", line),
			},
			other => panic!("expected a type, found {other:?} at line {}", line),
		}
	}

	fn parse_primary(&mut self) -> Expr {
		let line = self.peek_line();
		match self.advance().kind.clone() {
			TokenKind::IntLiteral(n) => Expr::IntLiteral(n),
			TokenKind::True => Expr::BoolLiteral(true),
			TokenKind::False => Expr::BoolLiteral(false),

			TokenKind::Identifier(name) => {
				if self.check(&TokenKind::LParen) {
					self.advance();
					let mut args = Vec::new();
					if !self.check(&TokenKind::RParen) {
						loop {
							args.push(self.parse_expression());
							if self.check(&TokenKind::Comma) { self.advance(); } else { break; }
						}
					}
					self.expect(TokenKind::RParen);
					Expr::Call { name, args }
				} else {
					Expr::Identifier(name)
				}
			}

			TokenKind::LParen => {
				let expr = self.parse_expression();
				self.expect(TokenKind::RParen);
				expr
			}

			other => panic!("unexpected token in expression: {:?} at line {}", other, line),
		}
	}

	fn parse_let(&mut self) -> Stmt {
		self.expect(TokenKind::Let);
		let mutable = if self.check(&TokenKind::Mut) {
			self.advance();
			true
		} else { false };

		let line = self.peek_line();
		let name = match self.advance().kind.clone() {
			TokenKind::Identifier(name) => name,
			other => panic!("expected identifier, found {:?} at line {}", other, line),
		};

		let ty = if self.check(&TokenKind::Colon) {
			self.advance();
			Some(self.parse_type())
		} else { None };
		self.expect(TokenKind::Eq);
		let value = self.parse_expression();
		self.expect(TokenKind::Semicolon);
		Stmt::Let { name, mutable, ty, value }
	}

	fn parse_block(&mut self) -> Vec<Stmt> {
		self.expect(TokenKind::LBrace);
		let mut stmts = Vec::new();
		while !self.check(&TokenKind::RBrace) {
			stmts.push(self.parse_statement());
		}
		self.expect(TokenKind::RBrace);
		stmts
	}

	fn parse_statement(&mut self) -> Stmt {
		match self.peek() {
			TokenKind::Let => self.parse_let(),
			TokenKind::If => self.parse_if(),
			TokenKind::While => self.parse_while(),
			TokenKind::Return => self.parse_return(),
			_ => {
				let expr = self.parse_expression();
				self.expect(TokenKind::Semicolon);
				Stmt::ExprStmt(expr)
			}
		}
	}

	fn parse_if(&mut self) -> Stmt {
		self.expect(TokenKind::If);
		let cond = self.parse_expression();
		let then_branch = self.parse_block();
		let else_branch = if self.check(&TokenKind::Else) {
			self.advance();
			Some(self.parse_block())
		} else {
			None
		};
		Stmt::If { cond, then_branch, else_branch }
	}

	fn parse_while(&mut self) -> Stmt {
		self.expect(TokenKind::While);
		let cond = self.parse_expression();
		let body = self.parse_block();
		Stmt::While { cond, body }
	}

	fn parse_return(&mut self) -> Stmt {
		self.expect(TokenKind::Return);
		let value = if self.check(&TokenKind::Semicolon) {
			None
		} else {
			Some(self.parse_expression())
		};
		self.expect(TokenKind::Semicolon);
		Stmt::Return(value)
	}

	fn parse_fn_decl(&mut self) -> Stmt {
		self.expect(TokenKind::Fn);

		let line = self.peek_line();
		let name = match self.advance().kind.clone() {
			TokenKind::Identifier(name) => name,
			other => panic!("expected function name, found {:?} at line {}", other, line),
		};

		self.expect(TokenKind::LParen);
		let mut params = Vec::new();
		if !self.check(&TokenKind::RParen) {
			loop {
				let param_line = self.peek_line();
				let param_name = match self.advance().kind.clone() {
					TokenKind::Identifier(name) => name,
					other => panic!("expected parameter name, found {:?} at line {}", other, param_line),
				};
				self.expect(TokenKind::Colon);
				let ty = self.parse_type();
				params.push(Param { name: param_name, ty });
				if self.check(&TokenKind::Comma) { self.advance(); } else { break; }
			}
		}
		self.expect(TokenKind::RParen);

		let return_ty = if self.check(&TokenKind::Arrow) {
			self.advance();
			Some(self.parse_type())
		} else { None };
		let body = self.parse_block();
		Stmt::FnDecl { name, params, return_ty, body }
	}

	// =============================================================================================
	// Weeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee
	// =============================================================================================
	fn parse_const_decl(&mut self) -> ConstDecl {
		self.expect(TokenKind::Const);

		let line = self.peek_line();
		let name = match self.advance().kind.clone() {
			TokenKind::Identifier(name) => name,
			other => panic!("expected const name, found {:?} at line {}", other, line),
		};

		self.expect(TokenKind::Colon);
		let ty = self.parse_type();
		self.expect(TokenKind::Eq);
		let expr = self.parse_expression();
		self.expect(TokenKind::Semicolon);

		let value = eval_const_expr(&expr, &self.const_values, line);
		self.const_values.insert(name.clone(), value);
		ConstDecl { name, ty, value }
	}

	pub fn parse_program(&mut self) -> Program {
		let mut consts = Vec::new();
		let mut functions = Vec::new();
		while !self.check(&TokenKind::Eof) {
			let line = self.peek_line();
			match self.peek() {
				TokenKind::Const => consts.push(self.parse_const_decl()),
				TokenKind::Fn => functions.push(self.parse_fn_decl()),
				other => panic!("expected 'const' or 'fn' at top level, found {:?} at line {}", other, line),
			}
		}
		Program { consts, functions }
	}

	fn peek_line(&self) -> usize {
		self.tokens[self.pos].line
	}
}

fn binop_info(kind: &TokenKind) -> Option<(BinOp, u8)> {
	use TokenKind::*;
	Some(match kind {
		Plus => (BinOp::Add, 9),
		Minus => (BinOp::Sub, 9),
		Shl => (BinOp::Shl, 8),
		Shr => (BinOp::Shr, 8),
		Amp => (BinOp::BitAnd, 7),
		Caret => (BinOp::BitXor, 6),
		Pipe => (BinOp::BitOr, 5),
		EqEq => (BinOp::Eq, 4),
		BangEq => (BinOp::NotEq, 4),
		Lt => (BinOp::Lt, 4),
		Gt => (BinOp::Gt, 4),
		LtEq => (BinOp::LtEq, 4),
		GtEq => (BinOp::GtEq, 4),
		AmpAmp => (BinOp::And, 3),
		PipePipe => (BinOp::Or, 2),
		_ => return None,
	})
}

fn eval_const_expr(expr: &Expr, known: &HashMap<String, i64>, line: usize) -> i64 {
	match expr {
		Expr::IntLiteral(n) => *n,
		Expr::Identifier(name) => *known.get(name).unwrap_or_else(|| panic!(
			"'{name}' isn't a previously-declared const at line {} — consts can only \
             reference EARLIER consts, top-to-bottom, no forward references", line
		)),
		Expr::Unary { op, expr } => {
			let v = eval_const_expr(expr, known, line);
			match op {
				UnOp::Neg => -v,
				UnOp::Not => !v
			}
		}
		Expr::Binary { op, left, right } => {
			let l = eval_const_expr(left, known, line);
			let r = eval_const_expr(right, known, line);
			match op {
				BinOp::Add => l + r,
				BinOp::Sub => l - r,
				BinOp::BitAnd => l & r,
				BinOp::BitOr => l | r,
				BinOp::BitXor => l ^ r,
				BinOp::Shl => l << r,
				BinOp::Shr => l >> r,
				other => panic!("'{:?}' doesn't produce a number and can't be used in a const expression at line {}", other, line),
			}
		}
		other => panic!("const expressions only support literals, earlier consts, and + - & | ^ << >> — found {:?} at line {}", other, line),
	}
}