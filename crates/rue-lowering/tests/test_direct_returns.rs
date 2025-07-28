use rue_ir::hir::{HirBlock, HirExpr, HirFunction, HirLiteral};
use rue_ir::types::RueType;
use rue_lexer::Span;
use rue_lowering::MirBuilder;

#[test]
fn test_direct_returns_in_if() {
    // Create a simple if expression that should use direct returns
    let hir_if = HirExpr::If {
        cond: Box::new(HirExpr::Literal {
            lit: HirLiteral::Bool(true),
            span: Span::dummy(),
        }),
        then_block: HirBlock {
            statements: vec![],
            expr: Some(Box::new(HirExpr::Literal {
                lit: HirLiteral::Int32(42),
                span: Span::dummy(),
            })),
        },
        else_block: Some(HirBlock {
            statements: vec![],
            expr: Some(Box::new(HirExpr::Literal {
                lit: HirLiteral::Int32(24),
                span: Span::dummy(),
            })),
        }),
        ty: RueType::I32,
        span: Span::dummy(),
    };

    let hir_func = HirFunction {
        name: "test".to_string(),
        params: vec![],
        return_type: RueType::I32,
        body: HirBlock {
            statements: vec![],
            expr: Some(Box::new(hir_if)),
        },
        span: Span::dummy(),
    };

    let mir_func = MirBuilder::lower_function(&hir_func);

    // Print the MIR for debugging
    eprintln!("Generated MIR for function 'test':");
    eprintln!("Entry block: {:?}", mir_func.entry_block);
    eprintln!("Number of blocks: {}", mir_func.blocks.len());

    for block in &mir_func.blocks {
        eprintln!("\nBlock {:?}:", block.id);
        eprintln!("  Parameters: {:?}", block.params);
        eprintln!("  Statements:");
        for stmt in &block.statements {
            eprintln!("    {stmt:?}");
        }
        eprintln!("  Terminator: {:?}", block.terminator);
    }

    // Verify we have exactly 3 blocks (entry, then, else)
    // No join block should be created with direct returns
    assert_eq!(mir_func.blocks.len(), 3);

    // Check that the then and else blocks have return terminators
    let then_block = &mir_func.blocks[1];
    let else_block = &mir_func.blocks[2];

    match &then_block.terminator {
        rue_ir::mir::MirTerminator::Return { value: Some(_), .. } => {}
        _ => panic!(
            "Expected return terminator in then block, got {:?}",
            then_block.terminator
        ),
    }

    match &else_block.terminator {
        rue_ir::mir::MirTerminator::Return { value: Some(_), .. } => {}
        _ => panic!(
            "Expected return terminator in else block, got {:?}",
            else_block.terminator
        ),
    }
}
