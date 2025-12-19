//! AST to RIR generation.
//!
//! AstGen converts the Abstract Syntax Tree into RIR instructions.
//! This is analogous to Zig's AstGen phase.

use rue_intern::Interner;
use rue_parser::{
    AssignTarget, Ast, BinaryOp, Expr, Function, Item, Statement, StructDecl, UnaryOp,
};

use crate::inst::{Inst, InstData, InstRef, Rir};

/// Generates RIR from an AST.
pub struct AstGen<'a> {
    /// The AST being processed
    ast: &'a Ast,
    /// String interner for symbols
    interner: &'a mut Interner,
    /// Output RIR
    rir: Rir,
}

impl<'a> AstGen<'a> {
    /// Create a new AstGen for the given AST.
    pub fn new(ast: &'a Ast, interner: &'a mut Interner) -> Self {
        Self {
            ast,
            interner,
            rir: Rir::new(),
        }
    }

    /// Generate RIR from the AST.
    pub fn generate(mut self) -> Rir {
        for item in &self.ast.items {
            self.gen_item(item);
        }
        self.rir
    }

    fn gen_item(&mut self, item: &Item) {
        match item {
            Item::Function(func) => {
                self.gen_function(func);
            }
            Item::Struct(struct_decl) => {
                self.gen_struct(struct_decl);
            }
        }
    }

    fn gen_struct(&mut self, struct_decl: &StructDecl) -> InstRef {
        let name = self.interner.intern(&struct_decl.name.name);
        let fields: Vec<_> = struct_decl
            .fields
            .iter()
            .map(|f| {
                let field_name = self.interner.intern(&f.name.name);
                let field_type = self.interner.intern(&f.ty.name);
                (field_name, field_type)
            })
            .collect();

        self.rir.add_inst(Inst {
            data: InstData::StructDecl { name, fields },
            span: struct_decl.span,
        })
    }

    fn gen_function(&mut self, func: &Function) -> InstRef {
        // Intern the function name and return type
        let name = self.interner.intern(&func.name.name);
        let return_type = self.interner.intern(&func.return_type.name);

        // Intern parameters
        let params: Vec<_> = func
            .params
            .iter()
            .map(|p| {
                let param_name = self.interner.intern(&p.name.name);
                let param_type = self.interner.intern(&p.ty.name);
                (param_name, param_type)
            })
            .collect();

        // Generate body expression
        let body = self.gen_expr(&func.body);

        // Create function declaration instruction
        self.rir.add_inst(Inst {
            data: InstData::FnDecl {
                name,
                params,
                return_type,
                body,
            },
            span: func.span,
        })
    }

    fn gen_expr(&mut self, expr: &Expr) -> InstRef {
        match expr {
            Expr::Int(lit) => self.rir.add_inst(Inst {
                data: InstData::IntConst(lit.value),
                span: lit.span,
            }),
            Expr::Bool(lit) => self.rir.add_inst(Inst {
                data: InstData::BoolConst(lit.value),
                span: lit.span,
            }),
            Expr::Ident(ident) => {
                let name = self.interner.intern(&ident.name);
                self.rir.add_inst(Inst {
                    data: InstData::VarRef { name },
                    span: ident.span,
                })
            }
            Expr::Binary(bin) => {
                let lhs = self.gen_expr(&bin.left);
                let rhs = self.gen_expr(&bin.right);
                let data = match bin.op {
                    BinaryOp::Add => InstData::Add { lhs, rhs },
                    BinaryOp::Sub => InstData::Sub { lhs, rhs },
                    BinaryOp::Mul => InstData::Mul { lhs, rhs },
                    BinaryOp::Div => InstData::Div { lhs, rhs },
                    BinaryOp::Mod => InstData::Mod { lhs, rhs },
                    BinaryOp::Eq => InstData::Eq { lhs, rhs },
                    BinaryOp::Ne => InstData::Ne { lhs, rhs },
                    BinaryOp::Lt => InstData::Lt { lhs, rhs },
                    BinaryOp::Gt => InstData::Gt { lhs, rhs },
                    BinaryOp::Le => InstData::Le { lhs, rhs },
                    BinaryOp::Ge => InstData::Ge { lhs, rhs },
                    BinaryOp::And => InstData::And { lhs, rhs },
                    BinaryOp::Or => InstData::Or { lhs, rhs },
                };
                self.rir.add_inst(Inst {
                    data,
                    span: bin.span,
                })
            }
            Expr::Unary(un) => {
                let operand = self.gen_expr(&un.operand);
                let data = match un.op {
                    UnaryOp::Neg => InstData::Neg { operand },
                    UnaryOp::Not => InstData::Not { operand },
                };
                self.rir.add_inst(Inst {
                    data,
                    span: un.span,
                })
            }
            Expr::Paren(paren) => {
                // Parentheses are transparent in the IR - just generate the inner expression
                self.gen_expr(&paren.inner)
            }
            Expr::Block(block) => self.gen_block(block),
            Expr::If(if_expr) => {
                let cond = self.gen_expr(&if_expr.cond);
                let then_block = self.gen_block(&if_expr.then_block);
                let else_block = if_expr.else_block.as_ref().map(|b| self.gen_block(b));

                self.rir.add_inst(Inst {
                    data: InstData::Branch {
                        cond,
                        then_block,
                        else_block,
                    },
                    span: if_expr.span,
                })
            }
            Expr::While(while_expr) => {
                let cond = self.gen_expr(&while_expr.cond);
                let body = self.gen_block(&while_expr.body);
                self.rir.add_inst(Inst {
                    data: InstData::Loop { cond, body },
                    span: while_expr.span,
                })
            }
            Expr::Call(call) => {
                let name = self.interner.intern(&call.name.name);
                let args: Vec<_> = call.args.iter().map(|a| self.gen_expr(a)).collect();

                self.rir.add_inst(Inst {
                    data: InstData::Call { name, args },
                    span: call.span,
                })
            }
            Expr::Break(break_expr) => self.rir.add_inst(Inst {
                data: InstData::Break,
                span: break_expr.span,
            }),
            Expr::Continue(continue_expr) => self.rir.add_inst(Inst {
                data: InstData::Continue,
                span: continue_expr.span,
            }),
            Expr::StructLit(struct_lit) => {
                let type_name = self.interner.intern(&struct_lit.name.name);
                let fields: Vec<_> = struct_lit
                    .fields
                    .iter()
                    .map(|f| {
                        let field_name = self.interner.intern(&f.name.name);
                        let field_value = self.gen_expr(&f.value);
                        (field_name, field_value)
                    })
                    .collect();

                self.rir.add_inst(Inst {
                    data: InstData::StructInit { type_name, fields },
                    span: struct_lit.span,
                })
            }
            Expr::Field(field_expr) => {
                let base = self.gen_expr(&field_expr.base);
                let field = self.interner.intern(&field_expr.field.name);

                self.rir.add_inst(Inst {
                    data: InstData::FieldGet { base, field },
                    span: field_expr.span,
                })
            }
            Expr::IntrinsicCall(intrinsic) => {
                let name = self.interner.intern(&intrinsic.name.name);
                let args: Vec<_> = intrinsic.args.iter().map(|a| self.gen_expr(a)).collect();

                self.rir.add_inst(Inst {
                    data: InstData::Intrinsic { name, args },
                    span: intrinsic.span,
                })
            }
        }
    }

    fn gen_block(&mut self, block: &rue_parser::BlockExpr) -> InstRef {
        if block.statements.is_empty() {
            // No statements, just the final expression
            self.gen_expr(&block.expr)
        } else {
            // Collect all instruction refs for the block
            let mut inst_refs = Vec::new();

            // Generate all statements first
            for stmt in &block.statements {
                let inst_ref = self.gen_statement(stmt);
                inst_refs.push(inst_ref.as_u32());
            }

            // Generate the final expression
            let final_expr = self.gen_expr(&block.expr);
            inst_refs.push(final_expr.as_u32());

            // Store the refs in extra data
            let extra_start = self.rir.add_extra(&inst_refs);
            let len = inst_refs.len() as u32;

            self.rir.add_inst(Inst {
                data: InstData::Block { extra_start, len },
                span: block.span,
            })
        }
    }

    fn gen_statement(&mut self, stmt: &Statement) -> InstRef {
        match stmt {
            Statement::Let(let_stmt) => {
                let name = self.interner.intern(&let_stmt.name.name);
                let ty = let_stmt.ty.as_ref().map(|t| self.interner.intern(&t.name));
                let init = self.gen_expr(&let_stmt.init);
                self.rir.add_inst(Inst {
                    data: InstData::Alloc {
                        name,
                        is_mut: let_stmt.is_mut,
                        ty,
                        init,
                    },
                    span: let_stmt.span,
                })
            }
            Statement::Assign(assign) => {
                let value = self.gen_expr(&assign.value);
                match &assign.target {
                    AssignTarget::Var(ident) => {
                        let name = self.interner.intern(&ident.name);
                        self.rir.add_inst(Inst {
                            data: InstData::Assign { name, value },
                            span: assign.span,
                        })
                    }
                    AssignTarget::Field(field_expr) => {
                        let base = self.gen_expr(&field_expr.base);
                        let field = self.interner.intern(&field_expr.field.name);
                        self.rir.add_inst(Inst {
                            data: InstData::FieldSet { base, field, value },
                            span: assign.span,
                        })
                    }
                }
            }
            Statement::Expr(expr) => {
                // Expression statements are evaluated for side effects
                // The result is discarded, but we still return the InstRef
                self.gen_expr(expr)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rue_lexer::Lexer;
    use rue_parser::Parser;

    fn gen_rir(source: &str) -> (Rir, Interner) {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();

        let mut interner = Interner::new();
        let astgen = AstGen::new(&ast, &mut interner);
        let rir = astgen.generate();
        (rir, interner)
    }

    #[test]
    fn test_gen_simple_function() {
        let (rir, interner) = gen_rir("fn main() -> i32 { 42 }");

        // Should have 2 instructions: IntConst(42), FnDecl
        assert_eq!(rir.len(), 2);

        // Check the function declaration
        let (_, fn_inst) = rir.iter().last().unwrap();
        match &fn_inst.data {
            InstData::FnDecl {
                name,
                params,
                return_type,
                body,
            } => {
                assert_eq!(interner.get(*name), "main");
                assert!(params.is_empty());
                assert_eq!(interner.get(*return_type), "i32");
                // Body should be the int constant
                let body_inst = rir.get(*body);
                assert!(matches!(body_inst.data, InstData::IntConst(42)));
            }
            _ => panic!("expected FnDecl"),
        }
    }

    #[test]
    fn test_gen_addition() {
        let (rir, _) = gen_rir("fn main() -> i32 { 1 + 2 }");

        // Should have: IntConst(1), IntConst(2), Add, FnDecl
        assert_eq!(rir.len(), 4);

        // Check add instruction
        let add_inst = rir.get(InstRef::from_raw(2));
        match &add_inst.data {
            InstData::Add { lhs, rhs } => {
                assert!(matches!(rir.get(*lhs).data, InstData::IntConst(1)));
                assert!(matches!(rir.get(*rhs).data, InstData::IntConst(2)));
            }
            _ => panic!("expected Add"),
        }
    }

    #[test]
    fn test_gen_precedence() {
        let (rir, _) = gen_rir("fn main() -> i32 { 1 + 2 * 3 }");

        // Should have: IntConst(1), IntConst(2), IntConst(3), Mul, Add, FnDecl
        assert_eq!(rir.len(), 6);

        // Check that add is the body (mul is nested)
        let fn_inst = rir.iter().last().unwrap().1;
        match &fn_inst.data {
            InstData::FnDecl { body, .. } => {
                let body_inst = rir.get(*body);
                match &body_inst.data {
                    InstData::Add { lhs, rhs } => {
                        // lhs should be IntConst(1)
                        assert!(matches!(rir.get(*lhs).data, InstData::IntConst(1)));
                        // rhs should be Mul
                        assert!(matches!(rir.get(*rhs).data, InstData::Mul { .. }));
                    }
                    _ => panic!("expected Add"),
                }
            }
            _ => panic!("expected FnDecl"),
        }
    }

    #[test]
    fn test_gen_negation() {
        let (rir, _) = gen_rir("fn main() -> i32 { -42 }");

        // Should have: IntConst(42), Neg, FnDecl
        assert_eq!(rir.len(), 3);

        // Check neg instruction
        let neg_inst = rir.get(InstRef::from_raw(1));
        match &neg_inst.data {
            InstData::Neg { operand } => {
                assert!(matches!(rir.get(*operand).data, InstData::IntConst(42)));
            }
            _ => panic!("expected Neg"),
        }
    }

    #[test]
    fn test_gen_parens() {
        let (rir, _) = gen_rir("fn main() -> i32 { (1 + 2) * 3 }");

        // Should have: IntConst(1), IntConst(2), Add, IntConst(3), Mul, FnDecl
        // Parens don't generate instructions, they just affect evaluation order
        assert_eq!(rir.len(), 6);

        // Check that mul is the body (add is nested)
        let fn_inst = rir.iter().last().unwrap().1;
        match &fn_inst.data {
            InstData::FnDecl { body, .. } => {
                let body_inst = rir.get(*body);
                match &body_inst.data {
                    InstData::Mul { lhs, rhs } => {
                        // lhs should be Add
                        assert!(matches!(rir.get(*lhs).data, InstData::Add { .. }));
                        // rhs should be IntConst(3)
                        assert!(matches!(rir.get(*rhs).data, InstData::IntConst(3)));
                    }
                    _ => panic!("expected Mul"),
                }
            }
            _ => panic!("expected FnDecl"),
        }
    }

    #[test]
    fn test_gen_all_binary_ops() {
        // Test all binary operators generate correct instructions
        let (rir, _) = gen_rir("fn main() -> i32 { 1 + 2 }");
        assert!(matches!(
            rir.get(InstRef::from_raw(2)).data,
            InstData::Add { .. }
        ));

        let (rir, _) = gen_rir("fn main() -> i32 { 1 - 2 }");
        assert!(matches!(
            rir.get(InstRef::from_raw(2)).data,
            InstData::Sub { .. }
        ));

        let (rir, _) = gen_rir("fn main() -> i32 { 1 * 2 }");
        assert!(matches!(
            rir.get(InstRef::from_raw(2)).data,
            InstData::Mul { .. }
        ));

        let (rir, _) = gen_rir("fn main() -> i32 { 1 / 2 }");
        assert!(matches!(
            rir.get(InstRef::from_raw(2)).data,
            InstData::Div { .. }
        ));

        let (rir, _) = gen_rir("fn main() -> i32 { 1 % 2 }");
        assert!(matches!(
            rir.get(InstRef::from_raw(2)).data,
            InstData::Mod { .. }
        ));
    }

    #[test]
    fn test_gen_let_binding() {
        let (rir, interner) = gen_rir("fn main() -> i32 { let x = 42; x }");

        // Find the Alloc instruction
        let alloc_inst = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::Alloc { .. }));
        assert!(alloc_inst.is_some());

        let (_, inst) = alloc_inst.unwrap();
        match &inst.data {
            InstData::Alloc {
                name,
                is_mut,
                ty,
                init,
            } => {
                assert_eq!(interner.get(*name), "x");
                assert!(!is_mut);
                assert!(ty.is_none());
                assert!(matches!(rir.get(*init).data, InstData::IntConst(42)));
            }
            _ => panic!("expected Alloc"),
        }
    }

    #[test]
    fn test_gen_let_mut() {
        let (rir, interner) = gen_rir("fn main() -> i32 { let mut x = 10; x }");

        let alloc_inst = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::Alloc { .. }));
        assert!(alloc_inst.is_some());

        let (_, inst) = alloc_inst.unwrap();
        match &inst.data {
            InstData::Alloc { name, is_mut, .. } => {
                assert_eq!(interner.get(*name), "x");
                assert!(*is_mut);
            }
            _ => panic!("expected Alloc"),
        }
    }

    #[test]
    fn test_gen_var_ref() {
        let (rir, interner) = gen_rir("fn main() -> i32 { let x = 42; x }");

        // The body should be a Block (since there are statements)
        let fn_inst = rir.iter().last().unwrap().1;
        match &fn_inst.data {
            InstData::FnDecl { body, .. } => {
                let body_inst = rir.get(*body);
                match &body_inst.data {
                    InstData::Block { extra_start, len } => {
                        // Block contains: Alloc, VarRef
                        assert_eq!(*len, 2);
                        let inst_refs = rir.get_extra(*extra_start, *len);
                        // Last instruction in block is the VarRef
                        let var_ref_inst = rir.get(InstRef::from_raw(inst_refs[1]));
                        match &var_ref_inst.data {
                            InstData::VarRef { name } => {
                                assert_eq!(interner.get(*name), "x");
                            }
                            _ => panic!("expected VarRef"),
                        }
                    }
                    _ => panic!("expected Block, got {:?}", body_inst.data),
                }
            }
            _ => panic!("expected FnDecl"),
        }
    }

    #[test]
    fn test_gen_assignment() {
        let (rir, interner) = gen_rir("fn main() -> i32 { let mut x = 10; x = 20; x }");

        // Find the Assign instruction
        let assign_inst = rir
            .iter()
            .find(|(_, inst)| matches!(inst.data, InstData::Assign { .. }));
        assert!(assign_inst.is_some());

        let (_, inst) = assign_inst.unwrap();
        match &inst.data {
            InstData::Assign { name, value } => {
                assert_eq!(interner.get(*name), "x");
                assert!(matches!(rir.get(*value).data, InstData::IntConst(20)));
            }
            _ => panic!("expected Assign"),
        }
    }

    #[test]
    fn test_gen_multiple_statements() {
        let (rir, _interner) = gen_rir("fn main() -> i32 { let x = 1; let y = 2; x + y }");

        // Count Alloc instructions
        let alloc_count = rir
            .iter()
            .filter(|(_, inst)| matches!(inst.data, InstData::Alloc { .. }))
            .count();
        assert_eq!(alloc_count, 2);

        // Check the body is a Block containing the allocs and the Add
        let fn_inst = rir.iter().last().unwrap().1;
        match &fn_inst.data {
            InstData::FnDecl { body, .. } => {
                let body_inst = rir.get(*body);
                match &body_inst.data {
                    InstData::Block { extra_start, len } => {
                        // Block contains: Alloc(x), Alloc(y), Add
                        assert_eq!(*len, 3);
                        let inst_refs = rir.get_extra(*extra_start, *len);
                        // Last instruction in block is the Add
                        let add_inst = rir.get(InstRef::from_raw(inst_refs[2]));
                        assert!(matches!(add_inst.data, InstData::Add { .. }));
                    }
                    _ => panic!("expected Block"),
                }
            }
            _ => panic!("expected FnDecl"),
        }
    }
}
