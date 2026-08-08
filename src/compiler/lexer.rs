//@formatter:off
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
	IntLiteral(i64),
	Identifier(String),   // covers var names AND type names (i16/i32/u16/u32/etc.)

	// keywords
	Let, Mut, Fn, If, Else, While, Return, As,

	// operators
	Plus, Minus, Star, Slash, Percent, Amp, Pipe, Caret,
	Eq, EqEq, Bang, BangEq,
	Lt, Gt, LtEq, GtEq, // `<`, `>`, `<=`, `>=`
	Shl, Shr, ShlEq, ShrEq, // `<<`, `>>`, `<<=`, `>>=`
	AmpAmp, PipePipe, // `&&`, `||`
	AmpEq, PipeEq, CaretEq, PercentEq, StarEq, SlashEq, PlusEq, MinusEq, // compound assignment
	Arrow, FatArrow, // `->`, `=>`
	ColonColon, // `::`
	Dot, // `.`
	Colon,     // : for type annotations
	Question, At, // `?`, `@`

	// punctuation
	LParen, RParen, LBrace, RBrace, LBracket, RBracket, Semicolon, Comma,

	True, False,

	Eof,
}
//@formatter:on

pub struct Token {
	pub kind: TokenKind,
	pub line: usize,
	pub col: usize,
}

pub struct Lexer<'a> {
	chars: std::iter::Peekable<std::str::Chars<'a>>,
	line: usize,
	col: usize,
}

impl<'a> Lexer<'a> {
	fn new(source: &'a str) -> Self {
		Lexer {
			chars: source.chars().peekable(),
			line: 1,
			col: 1,
		}
	}

	fn peek(&mut self) -> Option<char> {
		self.chars.peek().copied()
	}

	/// 1-indexed ... more intuitive for me
	fn peek_count(&self, advance_amount: usize) -> Option<char> {
		assert_ne!(advance_amount, 0);
		self.chars.clone().nth(advance_amount - 1)
	}

	fn advance(&mut self) -> Option<char> {
		let c = self.chars.next();
		if let Some(ch) = c {
			if ch == '\n' {
				self.line += 1;
				self.col = 1;
			} else {
				self.col += 1;
			}
		}
		c
	}

	fn skip_whitespace_and_comments(&mut self) {
		loop {
			match self.peek() {
				Some(c) if c.is_whitespace() => {
					self.advance();
				}
				Some('/') if self.peek_count(2) == Some('/') => {
					while let Some(c) = self.peek() {
						if c == '\n' { break; }
						self.advance();
					}
				}
				Some('/') if self.peek_count(2) == Some('*') => {
					self.advance();
					self.advance();
					loop {
						match self.peek() {
							None => break,
							Some('*') if self.peek_count(2) == Some('/') => {
								self.advance();
								self.advance();
								break;
							}
							Some(_) => {
								self.advance();
							}
						}
					}
				}
				_ => break,
			}
		}
	}

	fn scan_identifier(&mut self, first: char, line: usize, col: usize) -> Token {
		let mut text = String::from(first);
		while let Some(c) = self.peek() {
			if c.is_alphanumeric() || c == '_' {
				text.push(c);
				self.advance();
			} else {
				break;
			}
		}

		let kind = match text.as_str() {
			"let" => TokenKind::Let,
			"if" => TokenKind::If,
			"else" => TokenKind::Else,
			"while" => TokenKind::While,
			"return" => TokenKind::Return,
			"mut" => TokenKind::Mut,
			"as" => TokenKind::As,
			"fn" => TokenKind::Fn,
			"true" => TokenKind::True,
			"false" => TokenKind::False,
			_ => TokenKind::Identifier(text),
		};

		Token { kind, line, col }
	}

	fn scan_number(&mut self, first: char, line: usize, col: usize) -> Token {
		let mut text = String::from(first);
		while let Some(c) = self.peek() {
			if c.is_ascii_digit() {
				text.push(c);
				self.advance();
			} else {
				break;
			}
		}

		let kind = TokenKind::IntLiteral(text.parse().unwrap());

		Token { kind, line, col }
	}

	fn next_token(&mut self) -> Token {
		self.skip_whitespace_and_comments();

		let (line, col) = (self.line, self.col);
		let c = match self.advance() {
			None => return Token { kind: TokenKind::Eof, line, col },
			Some(c) => c
		};

		let kind = match c {
			'(' => TokenKind::LParen,
			')' => TokenKind::RParen,
			'{' => TokenKind::LBrace,
			'}' => TokenKind::RBrace,
			'[' => TokenKind::LBracket,
			']' => TokenKind::RBracket,
			';' => TokenKind::Semicolon,
			',' => TokenKind::Comma,
			'?' => TokenKind::Question,
			'@' => TokenKind::At,
			'.' => TokenKind::Dot,

			':' => {
				if self.peek() == Some(':') {
					self.advance();
					TokenKind::ColonColon
				} else {
					TokenKind::Colon
				}
			}

			'+' => {
				if self.peek() == Some('=') {
					self.advance();
					TokenKind::PlusEq
				} else {
					TokenKind::Plus
				}
			}
			'-' => {
				if self.peek() == Some('>') {
					self.advance();
					TokenKind::Arrow
				} else if self.peek() == Some('=') {
					self.advance();
					TokenKind::MinusEq
				} else {
					TokenKind::Minus
				}
			}
			'*' => {
				if self.peek() == Some('=') {
					self.advance();
					TokenKind::StarEq
				} else {
					TokenKind::Star
				}
			}
			'/' => {
				if self.peek() == Some('=') {
					self.advance();
					TokenKind::SlashEq
				} else {
					TokenKind::Slash
				}
			}
			'%' => {
				if self.peek() == Some('=') {
					self.advance();
					TokenKind::PercentEq
				} else {
					TokenKind::Percent
				}
			}
			'^' => {
				if self.peek() == Some('=') {
					self.advance();
					TokenKind::CaretEq
				} else {
					TokenKind::Caret
				}
			}

			'&' => {
				if self.peek() == Some('&') {
					self.advance();
					TokenKind::AmpAmp
				} else if self.peek() == Some('=') {
					self.advance();
					TokenKind::AmpEq
				} else {
					TokenKind::Amp
				}
			}
			'|' => {
				if self.peek() == Some('|') {
					self.advance();
					TokenKind::PipePipe
				} else if self.peek() == Some('=') {
					self.advance();
					TokenKind::PipeEq
				} else {
					TokenKind::Pipe
				}
			}

			'=' => {
				if self.peek() == Some('=') {
					self.advance();
					TokenKind::EqEq
				} else if self.peek() == Some('>') {
					self.advance();
					TokenKind::FatArrow
				} else {
					TokenKind::Eq
				}
			}
			'!' => {
				if self.peek() == Some('=') {
					self.advance();
					TokenKind::BangEq
				} else {
					TokenKind::Bang
				}
			}
			'<' => {
				if self.peek() == Some('<') && self.peek_count(2) == Some('=') {
					self.advance();
					self.advance();
					TokenKind::ShlEq
				} else if self.peek() == Some('<') {
					self.advance();
					TokenKind::Shl
				} else if self.peek() == Some('=') {
					self.advance();
					TokenKind::LtEq
				} else {
					TokenKind::Lt
				}
			}
			'>' => {
				if self.peek() == Some('>') && self.peek_count(2) == Some('=') {
					self.advance();
					self.advance();
					TokenKind::ShrEq
				} else if self.peek() == Some('>') {
					self.advance();
					TokenKind::Shr
				} else if self.peek() == Some('=') {
					self.advance();
					TokenKind::GtEq
				} else {
					TokenKind::Gt
				}
			}

			c if c.is_ascii_digit() => return self.scan_number(c, line, col),
			c if c.is_alphabetic() || c == '_' => return self.scan_identifier(c, line, col),
			_ => panic!("unexpected char '{c}' at {line}:{col}"),
		};

		Token { kind, line, col }
	}
}

fn tokenize(source: &str) -> Vec<Token> {
	let mut lexer: Lexer = Lexer::new(source);
	let mut tokens: Vec<Token> = Vec::new();

	loop {
		let token = lexer.next_token();
		let is_eof = token.kind == TokenKind::Eof;
		tokens.push(token);
		if is_eof {
			break;
		}
	}

	tokens
}

#[test]
fn tokenizes_simple_decl() {
	let tokens = tokenize("let x = 5;");
	assert_eq!(tokens[0].kind, TokenKind::Let);
	assert_eq!(tokens[1].kind, TokenKind::Identifier("x".into()));
	assert_eq!(tokens[2].kind, TokenKind::Eq);
	assert_eq!(tokens[3].kind, TokenKind::IntLiteral(5));
	assert_eq!(tokens[4].kind, TokenKind::Semicolon);
}

#[test]
fn tokenizes_bitwise_and_logical_ops() {
	let tokens = tokenize("& | ^ && || &= |= ^=");
	let expected = vec![
		TokenKind::Amp, TokenKind::Pipe, TokenKind::Caret,
		TokenKind::AmpAmp, TokenKind::PipePipe,
		TokenKind::AmpEq, TokenKind::PipeEq, TokenKind::CaretEq,
		TokenKind::Eof,
	];
	let kinds: Vec<_> = tokens.into_iter().map(|t| t.kind).collect();
	assert_eq!(kinds, expected);
}