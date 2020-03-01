use std::convert::{TryFrom, TryInto};

use super::*;
use crate::data::prelude::*;
use crate::data::ast::{Expr, ExprType};

enum BinaryPrecedence {
    Mul, Div, Mod,
    Add, Sub,
    Shl, Shr,
    Less, Greater, LessEq, GreaterEq,
    Eq, Ne,
    BitAnd,
    BitXor,
    BitOr,
    LogAnd,
    LogOr,
    Ternary, // TODO: will this work with pratt parsing?
    Assignment,
}

impl BinaryPrecedence {
    fn prec(&self) -> usize {
        use BinaryPrecedence::*;
        match self {
            Mul | Div | Mod => 0,
            Add | Sub => 1,
            Shl | Shr => 2,
            Less | Greater | LessEq | GreaterEq => 3,
            Eq | Ne => 4,
            BitAnd => 5,
            BitXor => 6,
            BitOr => 7,
            LogAnd => 8,
            LogOr => 9,
            Ternary => 10, // TODO: will this work with pratt parsing?
            Assignment => 11,
        }
    }
    fn left_associative(&self) -> bool {
        use BinaryPrecedence::*;
        match self {
            Ternary | Assignment => false,
            _ => true,
        }
    }
    fn constructor(&self) -> impl Fn(Expr, Expr) -> ExprType {
        use BinaryPrecedence::*;
        use ExprType::*;
        use crate::data::lex::ComparisonToken;
        let func = match self {
            Self::Mul => ExprType::Mul,
            Self::Div => ExprType::Div,
            Self::Mod => ExprType::Mod,
            Self::Add => ExprType::Add,
            Self::Sub => ExprType::Sub,
            Shl => |a, b| Shift(a, b, true),
            Shr => |a, b| Shift(a, b, false),
            Less => |a, b| Compare(a, b, ComparisonToken::Less),
            Greater => |a, b| Compare(a, b, ComparisonToken::Greater),
            LessEq => |a, b| Compare(a, b, ComparisonToken::LessEqual),
            GreaterEq => |a, b| Compare(a, b, ComparisonToken::GreaterEqual),
            Eq => |a, b| Compare(a, b, ComparisonToken::EqualEqual),
            Ne => |a, b| Compare(a, b, ComparisonToken::NotEqual),
            BitAnd => BitwiseAnd,
            BitXor => Xor,
            BitOr => BitwiseOr,
            LogAnd => LogicalAnd,
            LogOr => LogicalOr,
            Self::Ternary | Self::Assignment => panic!("lol no"),
        };
        move |a, b| func(Box::new(a), Box::new(b))
    }
}

impl TryFrom<&Token> for BinaryPrecedence {
    type Error = ();
    fn try_from(t: &Token) -> Result<BinaryPrecedence, ()> {
        use BinaryPrecedence::{*, self as Bin};
        use crate::data::lex::ComparisonToken::{*, self as Compare};
        use Token::*;
        Ok(match t {
            Star => Bin::Mul,
            Divide => Div,
            Token::Mod => Bin::Mod,
            Plus => Add,
            Minus => Sub,
            ShiftLeft => Shl,
            ShiftRight => Shr,
            Comparison(Compare::Less) => Bin::Less,
            Comparison(Compare::Greater) => Bin::Greater,
            Comparison(Compare::LessEqual) => Bin::LessEq,
            Comparison(Compare::GreaterEqual) => Bin::GreaterEq,
            Comparison(Compare::EqualEqual) => Bin::Eq,
            Comparison(Compare::NotEqual) => Bin::Ne,
            Ampersand => BitAnd,
            Xor => BitXor,
            BitwiseOr => BitOr,
            LogicalAnd => LogAnd,
            LogicalOr => LogOr,
            Token::Assignment(_) => Bin::Assignment,
            Question => Ternary,
            _ => return Err(())
        })
    }
}

impl<I: Iterator<Item = Lexeme>> Parser<I> {
    #[inline]
    pub fn expr(&mut self) -> SyntaxResult<Expr> {
        self.binary_expr(0)
    }
    fn binary_expr(&mut self, max_precedence: usize) -> SyntaxResult<Expr> {
        let mut expr = self.unary_expr()?;
        while let Some(binop) = self.peek_token()
                                    .and_then(|tok| BinaryPrecedence::try_from(tok).ok())
        {
            let prec = binop.prec();
            if prec < max_precedence {
                break;
            }
            // by some strange coincidence, the left associative ones are exactly the ones that `constructor` works for
            if binop.left_associative() {
                let constructor = binop.constructor();
                let right = self.binary_expr(prec + 1)?;
                let location = expr.location.merge(&right.location);
                expr = location.with(constructor(expr, right));
            } else {
                panic!("not implemented: =, ?, etc.")
            }
        }
        Ok(expr)
    }
    // | '(' expr ')'
    // | unary_operator unary_expr
    // | "sizeof" '(' type_name ')'
    // | "sizeof" unary_expr
    // | "++" unary_expr
    // | "--" unary_expr
    // | ID
    // | LITERAL
    fn unary_expr(&mut self) -> SyntaxResult<Expr> {
        if let Some(paren) = self.match_next(&Token::LeftParen) {
            let mut inner = self.expr()?;
            let end_loc = self.expect(Token::RightParen)?.location;
            inner.location = paren.location.merge(&end_loc);
            Ok(inner)
        } else if let Some(Locatable { data: constructor, location }) = self.match_unary_operator() {
            println!("saw unary operator");
            let inner = self.unary_expr()?;
            let location = location.merge(&inner.location);
            Ok(location.with(constructor(inner)))
        } else if let Some(loc) = self.match_id() {
            Ok(loc.map(ExprType::Id))
        } else if let Some(literal) = self.match_literal() {
            Ok(literal.map(ExprType::Literal))
        // TODO: cast expression, sizeof, ++, --
        // that will require distinguishing precedence for unary ops too
        } else {
            Err(self.next_location().with(SyntaxError::MissingPrimary))
        }
    }
    // '*' | '~' | '!' | '+' | '-' | '&'
    fn match_unary_operator(&mut self) -> Option<Locatable<impl Fn(Expr) -> ExprType>> {
        //Some(Locatable::new(|e| ExprType::Deref(Box::new(e)), self.last_location))
        let func = match self.peek_token()? {
            Token::Star => ExprType::Deref,
            Token::BinaryNot => ExprType::BitwiseNot,
            Token::LogicalNot => ExprType::LogicalNot,
            Token::Plus => ExprType::UnaryPlus,
            Token::Minus => ExprType::Negate,
            Token::Ampersand => ExprType::AddressOf,
            _ => return None,
        };
        let loc = self.next_token().unwrap().location;
        Some(Locatable::new(move |e| func(Box::new(e)), loc))
    }
}

#[cfg(test)]
mod test {
    use crate::test::*;
    use crate::*;
    use crate::SyntaxResult;
    use crate::data::ast::{Expr, ExprType};

    fn assert_same(left: &str, right: &str) {
        assert_eq!(parse_all(left), parse_all(right))
    }

    fn expr(e: &str) -> SyntaxResult<Expr> {
        parser(e).expr()
    }

    #[test]
    fn parse_unary() {
        let expr_data = |s| expr(s).unwrap().data;
        let x = || {
            Box::new(Location::default().with(ExprType::Id("x".into())))
        };
        fn int() -> Box<Expr> {
            Box::new(Location::default().with(ExprType::Literal(Literal::Int(1))))
        }
        fn assert_unary_int(s: &str, c: impl Fn(Box<Expr>) -> ExprType) {
            assert_eq!(expr(s).unwrap().data, c(int()));
        }
        assert_unary_int("1", |i| i.data);
        assert_unary_int("(((((1)))))", |i| i.data);
        assert_unary_int("+(1)", ExprType::UnaryPlus);
        assert_unary_int("-((1))", ExprType::Negate);
        assert_unary_int("*1", ExprType::Deref);
        assert_unary_int("~1", ExprType::BitwiseNot);
        assert_unary_int("!1", ExprType::LogicalNot);
        assert_unary_int("&1", ExprType::AddressOf);

        assert_eq!(expr_data("x"), x().data);
        assert_eq!(expr_data("x"), x().data);
        assert_eq!(expr_data("(((((x)))))"), x().data);
        assert_eq!(expr_data("+(x)"), ExprType::UnaryPlus(x()));
        assert_eq!(expr_data("-((x))"), ExprType::Negate(x()));
        assert_eq!(expr_data("*x"), ExprType::Deref(x()));
        assert_eq!(expr_data("~x"), ExprType::BitwiseNot(x()));
        assert_eq!(expr_data("!x"), ExprType::LogicalNot(x()));
        assert_eq!(expr_data("&x"), ExprType::AddressOf(x()));
    }
}