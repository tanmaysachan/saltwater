#![allow(unused_variables)]

use std::collections::{HashMap, VecDeque};

use counter::Counter;

use crate::data::{self, error::Warning, hir::*, *};
use crate::intern::InternedStr;

pub(crate) type TagScope = Scope<InternedStr, TagEntry>;
type Parser = super::Parser<super::Lexer>;
type SemanticResult<T> = Result<T, Locatable<SemanticError>>;

#[derive(Clone, Debug)]
pub(crate) enum TagEntry {
    Struct(StructRef),
    Union(StructRef),
    // list of (name, value)s
    Enum(Vec<(InternedStr, i64)>),
}

pub struct Analyzer {
    declarations: Parser,
    // in case a `Declaration` has multiple declarators
    pending: VecDeque<CompileResult<Locatable<Declaration>>>,
    /// objects that are in scope
    /// C actually has 4 different scopes:
    /// 1. ordinary identifiers
    /// 2. tags
    /// 3. label names
    /// 4. members
    ///
    /// This holds the scope for ordinary identifiers: variables and typedefs
    scope: Scope<InternedStr, MetadataRef>,
    /// the compound types that have been declared (struct/union/enum)
    /// scope 2. from above
    tag_scope: TagScope,
    /// Internal API which makes it easier to return errors lazily
    error_handler: ErrorHandler,
}

impl Iterator for Analyzer {
    type Item = CompileResult<Locatable<Declaration>>;
    fn next(&mut self) -> Option<Self::Item> {
        use ast::ExternalDeclaration;

        // like `try!`, but returns `Option<Result>` instead of `Result`
        macro_rules! ret_err {
            ($maybe_err: expr) => {
                match $maybe_err {
                    Err(err) => return Some(Err(err.into())),
                    Ok(success) => success,
                }
            };
        }

        if let Some(decl) = self.pending.pop_front() {
            return Some(decl);
        }
        let next = ret_err!(self.declarations.next()?);
        match next.data {
            ExternalDeclaration::Function(func) => {
                let id = func.id;
                let (meta_ref, body) = ret_err!(FunctionAnalyzer::analyze(
                    func,
                    &mut self.scope,
                    &mut self.tag_scope,
                    &mut self.error_handler,
                    next.location
                ));
                self.scope.insert(id, meta_ref);
                let decl = Declaration {
                    symbol: meta_ref,
                    init: Some(Initializer::FunctionBody(body)),
                };
                Some(Ok(Locatable::new(decl, next.location)))
            }
            ExternalDeclaration::Declaration(_) => unimplemented!("declarations"),
        }
    }
}

impl Analyzer {
    pub fn new(parser: Parser) -> Self {
        Self {
            declarations: parser,
            error_handler: ErrorHandler::new(),
            scope: Scope::new(),
            tag_scope: Scope::new(),
            pending: VecDeque::new(),
        }
    }
    fn parse_type(
        &mut self, specifiers: Vec<ast::DeclarationSpecifier>, decl: ast::DeclaratorType,
        location: Location,
    ) -> Result<ParsedType, SemanticError> {
        use ast::{DeclarationSpecifier::*, UnitSpecifier::*};
        use std::collections::HashSet;
        // need to parse specifiers now
        // it's not enough to collect into a `Set` since `long long` has a different meaning than `long`
        // instead, we see how many times each specifier is present
        // however, for some specifiers this doesn't really make sense:
        // if we see `struct s { int i; }` twice in a row,
        // it's more likely that the user forgot a semicolon in between than try to make some weird double struct type.
        // so: count the specifiers that are keywords and store the rest somewhere out of the way

        // TODO: initialization is a mess
        let (counter, compounds) = count_specifiers(specifiers, &mut self.error_handler, location);
        // Now that we've separated this into unit specifiers and compound specifiers,
        // see if we can pick up the proper types and qualifiers.
        let signed = match (counter.get(&Signed), counter.get(&Unsigned)) {
            (None, None) | (Some(_), None) => true,
            (None, Some(_)) => false,
            (Some(_), Some(_)) => {
                self.error_handler
                    .error(SemanticError::ConflictingSigned, location);
                true
            }
        };
        // `long` is special because of `long long`
        let mut ctype = None;
        if let Some(&long_count) = counter.get(&Long) {
            match long_count {
                0 => panic!("constraint violation, should only set count if > 0"),
                1 => ctype = Some(Type::Long(signed)),
                2 => unimplemented!("long long is also too long for rcc apparently"),
                _ => {
                    self.error_handler
                        .error(SemanticError::TooLong(long_count), location);
                    ctype = Some(Type::Long(signed));
                }
            }
        }
        let qualifiers = Qualifiers {
            c_const: counter.get(&Const).is_some(),
            volatile: counter.get(&Volatile).is_some(),
            func: FunctionQualifiers {
                inline: counter.get(&Inline).is_some(),
                no_return: counter.get(&NoReturn).is_some(),
            },
        };
        let mut storage_class = None;
        for (spec, sc) in &[
            (Auto, StorageClass::Auto),
            (Register, StorageClass::Register),
            (Static, StorageClass::Static),
            (Extern, StorageClass::Extern),
        ] {
            if counter.get(spec).is_some() {
                if let Some(existing) = storage_class {
                    self.error_handler.error(
                        SemanticError::ConflictingStorageClass(existing, *sc),
                        location,
                    );
                }
                storage_class = Some(*sc);
            }
        }
        // TODO: maybe use `iter!` macro instead of `vec!` to avoid an allocation?
        // https://play.rust-lang.org/?gist=0535aa4f749a14cb1b28d658446f3c13
        for (spec, new_ctype) in vec![
            (Bool, Type::Bool),
            (Char, Type::Char(signed)),
            (Short, Type::Short(signed)),
            (Int, Type::Int(signed)),
            // already handled `long` when we handled `long long`
            (Float, Type::Float),
            (Double, Type::Double),
            (Void, Type::Void),
            (VaList, Type::VaList),
        ] {
            if counter.get(&spec).is_some() {
                if let Some(existing) = ctype {
                    self.error_handler.error(
                        SemanticError::ConflictingType(existing, new_ctype.clone()),
                        location,
                    );
                }
                ctype = Some(new_ctype);
            }
        }
        for compound in compounds {
            match compound {
                Unit(_) => unreachable!("already caught"),
                Typedef => {
                    if let Some(existing) = storage_class {
                        self.error_handler.error(
                            SemanticError::ConflictingStorageClass(existing, StorageClass::Typedef),
                            location,
                        );
                    }
                    storage_class = Some(StorageClass::Typedef);
                }
                Enum { .. } | Struct(_) | Union(_) => unimplemented!("compound types"),
            }
        }
        // Check to see if we had a conflicting `signed` specifier
        // Note we use `counter` instead of the `signed` bool
        // because we've already set the default and forgotten whether it was originally present.
        if counter.get(&Signed).is_some() {
            match &ctype {
                // unsigned int
                Some(Type::Char(_)) | Some(Type::Short(_)) | Some(Type::Int(_))
                | Some(Type::Long(_)) => {}
                // unsigned float
                Some(other) => {
                    let err = SemanticError::CannotBeSigned(other.clone());
                    self.error_handler.error(err, location);
                }
                // unsigned i
                None => ctype = Some(Type::Int(signed)),
            }
        }
        // i;
        let ctype = ctype.unwrap_or_else(|| {
            self.error_handler.warn(Warning::ImplicitInt, location);
            Type::Int(true)
        });
        // *i[]
        let ctype = self.parse_decl(decl, ctype, location);
        Ok(ParsedType {
            qualifiers,
            storage_class,
            ctype,
            // TODO: set this properly when I implement enum/struct/union
            declared_compound_type: false,
        })
    }
    /// Parse the declarator for a variable, given a starting type.
    /// e.g. for `int *p`, takes `start: Type::Int(true)` and returns `Type::Pointer(Type::Int(true))`
    fn parse_decl(&mut self, decl: ast::DeclaratorType, current: Type, location: Location) -> Type {
        use crate::data::ast::DeclaratorType::*;
        match decl {
            End => current,
            Pointer { to, qualifiers } => {
                use UnitSpecifier::*;

                let inner = self.parse_decl(*to, current, location);
                let (counter, compounds) =
                    count_specifiers(qualifiers, &mut self.error_handler, location);
                let qualifiers = Qualifiers {
                    c_const: counter.get(&Const).is_some(),
                    volatile: counter.get(&Volatile).is_some(),
                    func: FunctionQualifiers {
                        inline: counter.get(&Inline).is_some(),
                        no_return: counter.get(&NoReturn).is_some(),
                    },
                };
                for &q in counter.keys() {
                    if !q.is_qualifier() {
                        self.error_handler
                            .error(SemanticError::NotAQualifier(q.into()), location);
                    }
                }
                for spec in compounds {
                    self.error_handler
                        .error(SemanticError::NotAQualifier(spec), location);
                }
                Type::Pointer(Box::new(inner), qualifiers)
            }
            Array { of, size } => {
                let size = if let Some(expr) = size {
                    Some(self.parse_expr(*expr).const_fold())
                } else {
                    None
                };
                unimplemented!()
            }
            Function(func) => unimplemented!(),
        }
    }
    fn parse_expr(&mut self, expr: ast::Expr) -> Expr {
        match expr {
            _ => unimplemented!(),
        }
    }
    /*
    fn parse_id(&mut self, name: InternedStr) {
        match self.scope.get(&name) {
            None => {
                self.error_handler.push_back(CompileError::new(
                    SemanticError::UndeclaredVar(name).into(),
                    location,
                ));
                Ok(pretend_zero)
            }
            Some(symbol) => {
                if symbol.storage_class == StorageClass::Typedef {
                    self.error_handler.push_back(
                        location.error(SemanticError::TypedefInExpressionContext),
                    );
                    return Ok(pretend_zero);
                }
                if let Type::Enum(ident, members) = &symbol.ctype {
                    let enumerator = members.iter().find_map(|(member, value)| {
                        if name == *member {
                            Some(*value)
                        } else {
                            None
                        }
                    });
                    if let Some(e) = enumerator {
                        return Ok(Expr {
                            constexpr: true,
                            ctype: Type::Enum(*ident, members.clone()),
                            location,
                            lval: false,
                            expr: ExprType::Literal(Literal::Int(e)),
                        });
                    }
                }
                Ok(Expr::id(symbol, location))
            }
    }
    */
}
struct FunctionAnalyzer<'a> {
    /// the function we are currently compiling.
    /// used for checking return types
    metadata: FunctionData,
    /// objects that are in scope
    /// It's a reference instead of an owned scope since the global variables are passed in from the `Analyzer`.
    scope: &'a mut Scope<InternedStr, MetadataRef>,
    /// compound types that are in scope: structs, unions, and enums
    /// scope 2. from above
    tag_scope: &'a mut TagScope,
    /// used for recovering from semantic errors
    error_handler: &'a mut ErrorHandler,
}

#[derive(Debug)]
/// used to keep track of function metadata
/// while doing semantic analysis
struct FunctionData {
    /// the name of the function
    id: InternedStr,
    /// where the function was declared
    location: Location,
    /// the return type of the function
    return_type: Type,
}

impl<'a> FunctionAnalyzer<'a> {
    /// Performs semantic analysis on the function and adds it to `METADATA_STORE`.
    /// Returns the analyzed statements.
    fn analyze(
        func: ast::FunctionDefinition, global_scope: &'a mut Scope<InternedStr, MetadataRef>,
        tag_scope: &'a mut TagScope, error_handler: &mut ErrorHandler, location: Location,
    ) -> SemanticResult<(MetadataRef, Vec<Stmt>)> {
        let return_type: ParsedType = unimplemented!();
        /*
        let return_type = func
            .declarator
            .return_type
            .parse_type(func.specifiers, error_handler, location)
            .map_err(|err| location.with(err))?;
            */
        if return_type.qualifiers != Qualifiers::default() {
            error_handler.warn(
                Warning::FunctionQualifiersIgnored(return_type.qualifiers),
                location,
            );
        }
        let metadata = FunctionData {
            location,
            id: func.id,
            return_type: return_type.ctype,
        };
        assert!(global_scope.is_global());
        assert!(tag_scope.is_global());
        let analyzer = FunctionAnalyzer {
            metadata,
            scope: global_scope,
            tag_scope,
            error_handler,
        };
        unimplemented!("analyzing functions");
        //assert!(global_scope.is_global());
        //assert!(tag_scope.is_global());
    }
}

struct ParsedType {
    // needs to be option because the default varies greatly depending on the context
    storage_class: Option<StorageClass>,
    qualifiers: Qualifiers,
    ctype: Type,
    // TODO: this is fishy
    declared_compound_type: bool,
}

use ast::{DeclarationSpecifier, UnitSpecifier};

fn count_specifiers(
    specifiers: Vec<DeclarationSpecifier>, error_handler: &mut ErrorHandler, location: Location,
) -> (Counter<UnitSpecifier, usize>, Vec<DeclarationSpecifier>) {
    use DeclarationSpecifier::*;
    use UnitSpecifier::*;

    let mut counter = Counter::<_, usize>::new();
    let mut compounds = Vec::new();
    for spec in specifiers {
        match spec {
            Unit(u) => counter.update(std::iter::once(u)),
            _ => compounds.push(spec),
        }
    }
    for (&spec, &count) in counter.iter() {
        if spec != Long && count > 1 {
            error_handler.warn(Warning::DuplicateSpecifier(spec, count), location);
        }
    }
    (counter, compounds)
}

impl UnitSpecifier {
    fn is_qualifier(self) -> bool {
        use UnitSpecifier::*;
        match self {
            Const | Volatile | Restrict | Inline | NoReturn => true,
            _ => false,
        }
    }
}

/*
fn desugar(expr: ExprType) -> data::Expr {
    unimplemented!()
    /*
    match expr {
        ExprType::Assign(lval, rval, token) => {
            // lval += rval -> { tmp = &lval; *tmp = rval; }
        }
    }
    */
}
*/

/*
fn analyze(declaration: ExternalDeclaration) {

}
*/

#[cfg(test)]
pub(crate) mod test {
    use super::*;
    use crate::test::*;

    pub(crate) fn analyze_expr(s: &str) -> CompileResult<Expr> {
        // because we're a child module of parse, we can skip straight to `expr()`
        let mut p = parser(s);
        let exp = p.expr()?;
        if let Some(err) = p.error_handler.pop_front() {
            return Err(err);
        }
        let mut a = Analyzer::new(p);
        let e = a.parse_expr(exp);
        if let Some(err) = a.error_handler.pop_front() {
            return Err(err);
        }
        Ok(e)
    }
}