use rue_lexer::Token;

pub type TokenNode = Token;

#[derive(Debug, Clone, PartialEq)]
pub enum TypeNode {
    I32(TokenNode),
    I64(TokenNode),
    Bool(TokenNode),
    Unit,
    Struct(StructTypeNode),
    Tuple(TupleTypeNode),
    Array(ArrayTypeNode),
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructTypeNode {
    pub name: TokenNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TupleTypeNode {
    pub open_paren: TokenNode,
    pub types: Vec<TypeNode>,
    pub close_paren: TokenNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArrayTypeNode {
    pub open_bracket: TokenNode,
    pub element_type: Box<TypeNode>,
    pub semicolon: TokenNode,
    pub size: TokenNode, // Integer literal token
    pub close_bracket: TokenNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CstRoot {
    pub items: Vec<CstNode>,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CstNode {
    Function(Box<FunctionNode>),
    StructDefinition(Box<StructDefinitionNode>),
    Statement(Box<StatementNode>),
    Expression(ExpressionNode),
    Token(TokenNode),
    Error(ErrorNode),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionNode {
    pub fn_token: TokenNode,
    pub name: TokenNode,
    pub param_list: ParamListNode,
    pub return_type: Option<ReturnTypeNode>,
    pub body: BlockNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructDefinitionNode {
    pub struct_token: TokenNode,
    pub name: TokenNode,
    pub open_brace: TokenNode,
    pub fields: Vec<StructFieldDefNode>,
    pub close_brace: TokenNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructFieldDefNode {
    pub name: TokenNode,
    pub type_annotation: TypeAnnotationNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReturnTypeNode {
    pub arrow: TokenNode,
    pub ty: TypeNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParamListNode {
    pub open_paren: TokenNode,
    pub params: Vec<ParameterNode>,
    pub close_paren: TokenNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParameterNode {
    pub name: TokenNode,
    pub type_annotation: Option<TypeAnnotationNode>,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlockNode {
    pub open_brace: TokenNode,
    pub statements: Vec<StatementNode>,
    pub final_expr: Option<ExpressionNode>,
    pub close_brace: TokenNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StatementNode {
    Let(Box<LetStatementNode>),
    Assign(AssignStatementNode),
    Expression(ExpressionStatementNode),
    Return(ReturnStatementNode),
}

#[derive(Debug, Clone, PartialEq)]
pub struct LetStatementNode {
    pub let_token: TokenNode,
    pub name: TokenNode,
    pub type_annotation: Option<TypeAnnotationNode>,
    pub equals: TokenNode,
    pub value: ExpressionNode,
    pub semicolon: TokenNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeAnnotationNode {
    pub colon: TokenNode,
    pub ty: TypeNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AssignStatementNode {
    pub name: TokenNode,
    pub equals: TokenNode,
    pub value: ExpressionNode,
    pub semicolon: TokenNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExpressionStatementNode {
    pub expression: ExpressionNode,
    pub semicolon: TokenNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReturnStatementNode {
    pub return_token: TokenNode,
    pub expression: Option<ExpressionNode>,
    pub semicolon: TokenNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IfStatementNode {
    pub if_token: TokenNode,
    pub condition: ExpressionNode,
    pub then_block: BlockNode,
    pub else_clause: Option<ElseClauseNode>,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ElseClauseNode {
    pub else_token: TokenNode,
    pub body: ElseBodyNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ElseBodyNode {
    Block(Box<BlockNode>),
    If(Box<IfStatementNode>), // for else if
}

#[derive(Debug, Clone, PartialEq)]
pub struct WhileStatementNode {
    pub while_token: TokenNode,
    pub condition: ExpressionNode,
    pub body: BlockNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExpressionNode {
    Binary(BinaryExprNode),
    Unary(UnaryExprNode),
    Call(CallExprNode),
    If(Box<IfStatementNode>),
    While(Box<WhileStatementNode>),
    Identifier(TokenNode),
    Literal(TokenNode),
    StructLiteral(StructLiteralNode),
    TupleLiteral(TupleLiteralNode),
    ArrayLiteral(ArrayLiteralNode),
    FieldAccess(FieldAccessNode),
    ArrayAccess(ArrayAccessNode),
}

impl ExpressionNode {
    pub fn span(&self) -> rue_lexer::Span {
        match self {
            ExpressionNode::Binary(node) => node
                .trivia
                .leading
                .first()
                .map(|t| t.span)
                .unwrap_or_else(|| node.left.span()),
            ExpressionNode::Unary(node) => node
                .trivia
                .leading
                .first()
                .map(|t| t.span)
                .unwrap_or_else(|| node.operator.span),
            ExpressionNode::Call(node) => node
                .trivia
                .leading
                .first()
                .map(|t| t.span)
                .unwrap_or_else(|| node.function.span()),
            ExpressionNode::If(node) => node.if_token.span,
            ExpressionNode::While(node) => node.while_token.span,
            ExpressionNode::Identifier(token) => token.span,
            ExpressionNode::Literal(token) => token.span,
            ExpressionNode::StructLiteral(node) => node.name.span,
            ExpressionNode::TupleLiteral(node) => node.open_paren.span,
            ExpressionNode::ArrayLiteral(node) => node.open_bracket.span,
            ExpressionNode::FieldAccess(node) => node.base.span(),
            ExpressionNode::ArrayAccess(node) => node.base.span(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BinaryExprNode {
    pub left: Box<ExpressionNode>,
    pub operator: TokenNode,
    pub right: Box<ExpressionNode>,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UnaryExprNode {
    pub operator: TokenNode,
    pub operand: Box<ExpressionNode>,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CallExprNode {
    pub function: Box<ExpressionNode>,
    pub open_paren: TokenNode,
    pub args: Vec<ExpressionNode>,
    pub close_paren: TokenNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ErrorNode {
    pub tokens: Vec<TokenNode>,
    pub message: String,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Trivia {
    pub leading: Vec<TokenNode>,
    pub trailing: Vec<TokenNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructLiteralNode {
    pub name: TokenNode,
    pub open_brace: TokenNode,
    pub fields: Vec<StructFieldInitNode>,
    pub close_brace: TokenNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StructFieldInitNode {
    pub name: TokenNode,
    pub colon: TokenNode,
    pub value: ExpressionNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TupleLiteralNode {
    pub open_paren: TokenNode,
    pub elements: Vec<ExpressionNode>,
    pub close_paren: TokenNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArrayLiteralNode {
    pub open_bracket: TokenNode,
    pub elements: Vec<ExpressionNode>,
    pub close_bracket: TokenNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldAccessNode {
    pub base: Box<ExpressionNode>,
    pub dot: TokenNode,
    pub field: FieldKindNode,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FieldKindNode {
    Named(TokenNode),      // identifier
    Positional(TokenNode), // integer literal
}

#[derive(Debug, Clone, PartialEq)]
pub struct ArrayAccessNode {
    pub base: Box<ExpressionNode>,
    pub open_bracket: TokenNode,
    pub index: Box<ExpressionNode>,
    pub close_bracket: TokenNode,
    pub trivia: Trivia,
}
