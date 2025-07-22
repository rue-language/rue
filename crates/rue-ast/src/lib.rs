use rue_lexer::Token;

pub type TokenNode = Token;

#[derive(Debug, Clone, PartialEq)]
pub enum TypeNode {
    I32(TokenNode),
    I64(TokenNode),
    Bool(TokenNode),
    Unit,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CstRoot {
    pub items: Vec<CstNode>,
    pub trivia: Trivia,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CstNode {
    Function(Box<FunctionNode>),
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
    Let(LetStatementNode),
    Assign(AssignStatementNode),
    Expression(ExpressionStatementNode),
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
