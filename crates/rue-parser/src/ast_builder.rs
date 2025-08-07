//! CST to AST lowering
//!
//! This module converts the Concrete Syntax Tree (CST) with all its syntactic details
//! into the compact, cache-efficient Abstract Syntax Tree (AST) that uses the
//! Zig-inspired struct-of-arrays layout.
//!
//! The lowering process:
//! - Removes syntactic noise (trivia, parentheses, semicolons)
//! - Interns strings for deduplication
//! - Converts to compact node representation
//! - Preserves source spans for error reporting

use rue_ast::{
    ArrayAccessNode, ArrayLiteralNode, AssignStatementNode, BinaryExprNode, BlockNode,
    CallExprNode, CstNode, CstRoot, ExpressionNode, ExpressionStatementNode, FieldAccessNode,
    FunctionNode, IfStatementNode, LetStatementNode, ParameterNode, ReturnStatementNode,
    StatementNode, StructDefinitionNode, StructLiteralNode, TupleLiteralNode, UnaryExprNode,
    WhileStatementNode,
};
use rue_ir::ast::{Ast, AstBuilder, BinaryOp, NodeIndex, NodeTag, StringIndex, UnaryOp};
use rue_lexer::{Span, TokenKind};

/// Converts a CST to the compact AST representation
pub struct CstToAst {
    builder: AstBuilder,
}

impl Default for CstToAst {
    fn default() -> Self {
        Self::new()
    }
}

impl CstToAst {
    pub fn new() -> Self {
        Self {
            builder: AstBuilder::new(),
        }
    }

    /// Convert a CST root to AST
    pub fn convert(mut self, cst: CstRoot) -> Ast {
        // Process all top-level items
        for item in cst.items {
            match item {
                CstNode::Function(func) => {
                    self.convert_function(*func);
                }
                CstNode::StructDefinition(struct_def) => {
                    self.convert_struct_definition(*struct_def);
                }
                CstNode::Statement(_stmt) => {
                    // Top-level statements are not typically allowed
                    // Could add error handling here
                }
                CstNode::Expression(_expr) => {
                    // Top-level expressions are not typically allowed
                }
                CstNode::Token(_) | CstNode::Error(_) => {
                    // Skip tokens and errors
                }
            }
        }

        self.builder.finish()
    }

    /// Convert a function definition
    fn convert_function(&mut self, func: FunctionNode) -> NodeIndex {
        // Extract function name
        let name = self.intern_token_string(&func.name);

        // Convert parameters
        let params: Vec<NodeIndex> = func
            .param_list
            .params
            .iter()
            .map(|p| self.convert_parameter(p))
            .collect();

        // Convert return type if present
        let return_type = func
            .return_type
            .as_ref()
            .map(|ret_type| self.convert_type(&ret_type.ty));

        // Calculate span before moving body
        let span = Span {
            start: func.fn_token.span.start,
            end: func.body.close_brace.span.end,
        };

        // Convert body (this moves func.body)
        let body = self.convert_block(func.body);

        self.builder
            .build_function(name, params, return_type, body, span)
    }

    /// Convert a struct definition
    fn convert_struct_definition(&mut self, struct_def: StructDefinitionNode) -> NodeIndex {
        let name = self.intern_token_string(&struct_def.name);

        // Convert all fields
        let mut fields = Vec::new();
        for field in struct_def.fields {
            let field_name = self.intern_token_string(&field.name);
            let field_type = self.convert_type(&field.type_annotation.ty);
            fields.push((field_name, field_type));
        }

        let span = Span {
            start: struct_def.struct_token.span.start,
            end: struct_def.close_brace.span.end,
        };

        self.builder.build_struct_def(name, fields, span)
    }

    /// Convert a parameter
    fn convert_parameter(&mut self, param: &ParameterNode) -> NodeIndex {
        let name = self.intern_token_string(&param.name);

        // Convert the type annotation if present
        if let Some(type_ann) = &param.type_annotation {
            let _param_type = self.convert_type(&type_ann.ty);
            // Note: Currently we just convert the type to ensure it's in the AST
            // In a full implementation, we'd link parameter name to type
        }

        let span = param.name.span;
        // Create a simple parameter node (for now, just an identifier)
        // In the future, we might want a specific Param node type
        self.builder.build_identifier(name, span)
    }

    /// Convert a block
    fn convert_block(&mut self, block: BlockNode) -> NodeIndex {
        let mut statements = Vec::new();

        // Convert all statements
        for stmt in block.statements {
            statements.push(self.convert_statement(stmt));
        }

        // Convert final expression if present
        if let Some(expr) = block.final_expr {
            let expr_node = self.convert_expression(expr);
            statements.push(expr_node);
        }

        let span = Span {
            start: block.open_brace.span.start,
            end: block.close_brace.span.end,
        };

        self.builder.build_block(statements, span)
    }

    /// Convert a statement
    fn convert_statement(&mut self, stmt: StatementNode) -> NodeIndex {
        match stmt {
            StatementNode::Let(let_stmt) => self.convert_let_statement(*let_stmt),
            StatementNode::Assign(assign) => self.convert_assign_statement(assign),
            StatementNode::Expression(expr_stmt) => self.convert_expression_statement(expr_stmt),
            StatementNode::Return(ret) => self.convert_return_statement(ret),
        }
    }

    /// Convert a let statement
    fn convert_let_statement(&mut self, stmt: LetStatementNode) -> NodeIndex {
        let name = self.intern_token_string(&stmt.name);
        let init = self.convert_expression(stmt.value);

        // Use only the 'let' keyword span to match CST path behavior
        let span = stmt.let_token.span;

        // Build a let node
        // We'll use the pattern: name in token field, init in lhs
        let data = rue_ir::ast::NodeData {
            lhs: init.raw(),
            rhs: 0, // Could store type hint here in the future
        };

        self.builder.add_node(NodeTag::Let, span, name.raw(), data)
    }

    /// Convert an assignment statement
    fn convert_assign_statement(&mut self, stmt: AssignStatementNode) -> NodeIndex {
        let target = self.intern_token_string(&stmt.name);
        let value = self.convert_expression(stmt.value);

        // Use only the variable name span to match CST path behavior
        let span = stmt.name.span;

        // Build assignment node
        let data = rue_ir::ast::NodeData {
            lhs: value.raw(),
            rhs: 0,
        };

        self.builder
            .add_node(NodeTag::Assign, span, target.raw(), data)
    }

    /// Convert an expression statement
    fn convert_expression_statement(&mut self, stmt: ExpressionStatementNode) -> NodeIndex {
        let expr = self.convert_expression(stmt.expression);
        let span = stmt.semicolon.span;

        // Build expression statement node
        let data = rue_ir::ast::NodeData {
            lhs: expr.raw(),
            rhs: 0,
        };

        self.builder.add_node(NodeTag::ExprStmt, span, 0, data)
    }

    /// Convert a return statement
    fn convert_return_statement(&mut self, stmt: ReturnStatementNode) -> NodeIndex {
        let value = stmt.expression.map(|e| self.convert_expression(e));

        // Use only the return token span to match CST path behavior
        let span = stmt.return_token.span;

        // Build return node
        let data = rue_ir::ast::NodeData {
            lhs: value.map(|n| n.raw()).unwrap_or(0),
            rhs: 0,
        };

        self.builder.add_node(NodeTag::Return, span, 0, data)
    }

    /// Convert an expression
    fn convert_expression(&mut self, expr: ExpressionNode) -> NodeIndex {
        match expr {
            ExpressionNode::Binary(binary) => self.convert_binary(binary),
            ExpressionNode::Unary(unary) => self.convert_unary(unary),
            ExpressionNode::Call(call) => self.convert_call(call),
            ExpressionNode::If(if_expr) => self.convert_if(*if_expr),
            ExpressionNode::While(while_expr) => self.convert_while(*while_expr),
            ExpressionNode::Identifier(token) => {
                let name = self.intern_token_string(&token);
                self.builder.build_identifier(name, token.span)
            }
            ExpressionNode::Literal(token) => self.convert_literal(token),
            ExpressionNode::StructLiteral(struct_lit) => self.convert_struct_literal(struct_lit),
            ExpressionNode::TupleLiteral(tuple_lit) => self.convert_tuple_literal(tuple_lit),
            ExpressionNode::ArrayLiteral(array_lit) => self.convert_array_literal(array_lit),
            ExpressionNode::FieldAccess(field) => self.convert_field_access(field),
            ExpressionNode::ArrayAccess(array) => self.convert_array_access(array),
        }
    }

    /// Convert a binary expression
    fn convert_binary(&mut self, binary: BinaryExprNode) -> NodeIndex {
        let left = self.convert_expression(*binary.left);
        let right = self.convert_expression(*binary.right);
        let op = self.convert_binary_op(&binary.operator);

        // Use the operator token span (matches CST path behavior)
        let span = binary.operator.span;

        self.builder.build_binary(op, left, right, span)
    }

    /// Convert a unary expression
    fn convert_unary(&mut self, unary: UnaryExprNode) -> NodeIndex {
        let operand = self.convert_expression(*unary.operand);
        let op = self.convert_unary_op(&unary.operator);

        let span = unary.operator.span;

        // Store operator in extra_data
        let op_index = self.builder.add_extra_data(&[op as u32]);

        let data = rue_ir::ast::NodeData {
            lhs: operand.raw(),
            rhs: op_index,
        };

        self.builder.add_node(NodeTag::Unary, span, 0, data)
    }

    /// Convert a call expression
    fn convert_call(&mut self, call: CallExprNode) -> NodeIndex {
        let callee = self.convert_expression(*call.function);
        let args: Vec<NodeIndex> = call
            .args
            .into_iter()
            .map(|a| self.convert_expression(a))
            .collect();

        // Use the open paren span start (which is right after the function name)
        // This matches the CST path behavior
        let span = Span {
            start: call.open_paren.span.start,
            end: call.open_paren.span.start + 1, // Just the opening paren
        };

        self.builder.build_call(callee, args, span)
    }

    /// Convert an if expression
    fn convert_if(&mut self, if_stmt: IfStatementNode) -> NodeIndex {
        let condition = self.convert_expression(if_stmt.condition);
        let then_block = self.convert_block(if_stmt.then_block);

        let else_block = if_stmt
            .else_clause
            .map(|else_clause| match else_clause.body {
                rue_ast::ElseBodyNode::Block(block) => self.convert_block(*block),
                rue_ast::ElseBodyNode::If(if_stmt) => self.convert_if(*if_stmt),
            });

        let span = if_stmt.if_token.span;

        // Store all blocks in extra_data: [condition, then_block, has_else (0/1), else_block?]
        let mut extra_data = vec![condition.raw(), then_block.raw()];
        if let Some(else_node) = else_block {
            extra_data.push(1); // Has else
            extra_data.push(else_node.raw());
        } else {
            extra_data.push(0); // No else
        }

        let extra_start = self.builder.add_extra_data(&extra_data);

        let data = rue_ir::ast::NodeData {
            lhs: extra_start,
            rhs: extra_data.len() as u32,
        };

        self.builder.add_node(NodeTag::If, span, 0, data)
    }

    /// Convert a while expression
    fn convert_while(&mut self, while_stmt: WhileStatementNode) -> NodeIndex {
        let condition = self.convert_expression(while_stmt.condition);
        let body = self.convert_block(while_stmt.body);

        let span = while_stmt.while_token.span;

        let data = rue_ir::ast::NodeData {
            lhs: condition.raw(),
            rhs: body.raw(),
        };

        self.builder.add_node(NodeTag::While, span, 0, data)
    }

    /// Convert a literal
    fn convert_literal(&mut self, token: rue_lexer::Token) -> NodeIndex {
        let span = token.span;

        let (data, token_val) = match &token.kind {
            TokenKind::Integer(val) => {
                // Store integer value in data fields, use token field to mark as integer (0)
                (rue_ir::ast::NodeData::literal(*val as u64), 0)
            }
            TokenKind::True => {
                // Use token field to mark as boolean (1)
                (rue_ir::ast::NodeData::literal(1), 1)
            }
            TokenKind::False => {
                // Use token field to mark as boolean (1)
                (rue_ir::ast::NodeData::literal(0), 1)
            }
            _ => (rue_ir::ast::NodeData::EMPTY, 0),
        };

        self.builder
            .add_node(NodeTag::Literal, span, token_val, data)
    }

    /// Convert struct literal
    fn convert_struct_literal(&mut self, struct_lit: StructLiteralNode) -> NodeIndex {
        let name = self.intern_token_string(&struct_lit.name);

        // Store field initializers in extra_data
        let mut field_data = Vec::new();
        for field in struct_lit.fields {
            let field_name = self.intern_token_string(&field.name);
            let field_value = self.convert_expression(field.value);
            field_data.push(field_name.raw());
            field_data.push(field_value.raw());
        }

        let start = self.builder.add_extra_data(&field_data);
        let count = (field_data.len() / 2) as u32; // Number of fields

        let span = Span {
            start: struct_lit.name.span.start,
            end: struct_lit.close_brace.span.end,
        };

        let data = rue_ir::ast::NodeData::extra_range(start, count);
        self.builder
            .add_node(NodeTag::StructLiteral, span, name.raw(), data)
    }

    /// Convert tuple literal
    fn convert_tuple_literal(&mut self, tuple_lit: TupleLiteralNode) -> NodeIndex {
        let elements: Vec<NodeIndex> = tuple_lit
            .elements
            .into_iter()
            .map(|e| self.convert_expression(e))
            .collect();

        let element_data: Vec<u32> = elements.iter().map(|n| n.raw()).collect();
        let start = self.builder.add_extra_data(&element_data);

        let span = Span {
            start: tuple_lit.open_paren.span.start,
            end: tuple_lit.close_paren.span.end,
        };

        let data = rue_ir::ast::NodeData::extra_range(start, element_data.len() as u32);
        self.builder.add_node(NodeTag::TupleLiteral, span, 0, data)
    }

    /// Convert array literal
    fn convert_array_literal(&mut self, array_lit: ArrayLiteralNode) -> NodeIndex {
        let elements: Vec<NodeIndex> = array_lit
            .elements
            .into_iter()
            .map(|e| self.convert_expression(e))
            .collect();

        let element_data: Vec<u32> = elements.iter().map(|n| n.raw()).collect();
        let start = self.builder.add_extra_data(&element_data);

        let span = Span {
            start: array_lit.open_bracket.span.start,
            end: array_lit.close_bracket.span.end,
        };

        let data = rue_ir::ast::NodeData::extra_range(start, element_data.len() as u32);
        self.builder.add_node(NodeTag::ArrayLiteral, span, 0, data)
    }

    /// Convert field access
    fn convert_field_access(&mut self, field: FieldAccessNode) -> NodeIndex {
        let base = self.convert_expression(*field.base);

        // Extract the token from the FieldKindNode enum
        let (field_token, field_index) = match field.field {
            rue_ast::FieldKindNode::Named(token) => {
                // Named field access (e.g., obj.fieldName)
                let name = self.intern_token_string(&token);
                (token, name.raw())
            }
            rue_ast::FieldKindNode::Positional(token) => {
                // Positional/tuple field access (e.g., tuple.0)
                // Store the integer value directly
                let index = if let rue_lexer::TokenKind::Integer(val) = &token.kind {
                    *val as u32
                } else {
                    // Shouldn't happen if parser is correct
                    0
                };
                (token, index)
            }
        };

        let span = Span {
            start: field.dot.span.start, // Use dot position as start
            end: field_token.span.end,
        };

        let data = rue_ir::ast::NodeData {
            lhs: base.raw(),
            rhs: field_index,
        };

        self.builder.add_node(NodeTag::FieldAccess, span, 0, data)
    }

    /// Convert array access
    fn convert_array_access(&mut self, array: ArrayAccessNode) -> NodeIndex {
        let base = self.convert_expression(*array.base);
        let index = self.convert_expression(*array.index);

        // Use the bracket span for the access operation
        let span = Span {
            start: array.open_bracket.span.start,
            end: array.close_bracket.span.end,
        };

        let data = rue_ir::ast::NodeData {
            lhs: base.raw(),
            rhs: index.raw(),
        };

        self.builder.add_node(NodeTag::ArrayAccess, span, 0, data)
    }

    /// Convert a binary operator token to enum
    fn convert_binary_op(&self, token: &rue_lexer::Token) -> BinaryOp {
        match &token.kind {
            TokenKind::Plus => BinaryOp::Add,
            TokenKind::Minus => BinaryOp::Sub,
            TokenKind::Star => BinaryOp::Mul,
            TokenKind::Slash => BinaryOp::Div,
            TokenKind::Percent => BinaryOp::Mod,
            TokenKind::Equal => BinaryOp::Eq,     // == comparison
            TokenKind::NotEqual => BinaryOp::Ne,  // != comparison
            TokenKind::Less => BinaryOp::Lt,      // <
            TokenKind::LessEqual => BinaryOp::Le, // <=
            TokenKind::Greater => BinaryOp::Gt,   // >
            TokenKind::GreaterEqual => BinaryOp::Ge, // >=
            // Note: && and || operators not yet in lexer
            // When added, they would need new TokenKind variants:
            // TokenKind::AmpAmp => BinaryOp::And,
            // TokenKind::PipePipe => BinaryOp::Or,
            _ => {
                // This shouldn't happen if the parser is correct
                // Could add proper error handling here
                BinaryOp::Add
            }
        }
    }

    /// Convert a unary operator token to enum
    fn convert_unary_op(&self, token: &rue_lexer::Token) -> UnaryOp {
        match &token.kind {
            TokenKind::Minus => UnaryOp::Neg, // Unary negation
            // Note: The lexer doesn't currently support standalone '!' for logical NOT
            // It only supports '!=' for not-equal comparison
            // When logical NOT is added to the lexer, it would be:
            // TokenKind::Bang => UnaryOp::Not,
            _ => {
                // This shouldn't happen if the parser is correct
                // Could add proper error handling here
                UnaryOp::Neg
            }
        }
    }

    /// Convert a type node
    fn convert_type(&mut self, ty: &rue_ast::TypeNode) -> NodeIndex {
        use rue_ast::TypeNode;

        match ty {
            TypeNode::I32(token) => {
                let name = self.builder.intern_string("i32");
                self.builder.build_type(name, token.span)
            }
            TypeNode::I64(token) => {
                let name = self.builder.intern_string("i64");
                self.builder.build_type(name, token.span)
            }
            TypeNode::Bool(token) => {
                let name = self.builder.intern_string("bool");
                self.builder.build_type(name, token.span)
            }
            TypeNode::Unit => {
                // Unit type has no token, use a default span
                let name = self.builder.intern_string("()");
                self.builder.build_type(name, Span { start: 0, end: 0 })
            }
            TypeNode::Struct(struct_type) => {
                let name = self.intern_token_string(&struct_type.name);
                self.builder.build_struct_type(name, struct_type.name.span)
            }
            TypeNode::Tuple(tuple_type) => {
                let element_types: Vec<NodeIndex> = tuple_type
                    .types
                    .iter()
                    .map(|ty| self.convert_type(ty))
                    .collect();

                let span = Span {
                    start: tuple_type.open_paren.span.start,
                    end: tuple_type.close_paren.span.end,
                };

                self.builder.build_tuple_type(element_types, span)
            }
            TypeNode::Array(array_type) => {
                let element_type = self.convert_type(&array_type.element_type);

                // Extract the size from the literal token
                let size = if let rue_lexer::TokenKind::Integer(val) = &array_type.size.kind {
                    *val as u32
                } else {
                    // Should not happen if parser is correct
                    0
                };

                let span = Span {
                    start: array_type.open_bracket.span.start,
                    end: array_type.close_bracket.span.end,
                };

                self.builder.build_array_type(element_type, size, span)
            }
        }
    }

    /// Intern a token's string value
    fn intern_token_string(&mut self, token: &rue_lexer::Token) -> StringIndex {
        match &token.kind {
            TokenKind::Ident(name) => self.builder.intern_string(name),
            _ => self.builder.intern_string(""), // Should handle error
        }
    }
}

/// Public API for CST to AST conversion
pub fn lower_cst_to_ast(cst: CstRoot) -> Ast {
    let converter = CstToAst::new();
    converter.convert(cst)
}
