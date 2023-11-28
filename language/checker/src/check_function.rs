use std::sync::Arc;
use std::sync::Mutex;
use syntax::function::{CodeBody, CodelessFinalizedFunction, FinalizedCodeBody, FinalizedFunction, UnfinalizedFunction};
use syntax::{SimpleVariableManager, is_modifier, Modifier, ParsingError};
use syntax::async_util::NameResolver;
use syntax::code::{ExpressionType, FinalizedEffects, FinalizedExpression, FinalizedField, FinalizedMemberField};
use syntax::syntax::Syntax;
use syntax::types::FinalizedTypes;
use crate::finalize_generics;
use crate::check_code::{verify_code};
use crate::output::TypesChecker;
use data::tokens::{CodeErrorToken};

pub async fn verify_function(mut function: UnfinalizedFunction, syntax: &Arc<Mutex<Syntax>>,
                             include_refs: bool) -> Result<(CodelessFinalizedFunction, CodeBody), ParsingError> {
    let mut fields = Vec::new();
    for argument in &mut function.fields {
        let field = argument.await?;
        let mut field = FinalizedMemberField {
            modifiers: field.modifiers,
            attributes: field.attributes,
            field: FinalizedField { field_type: field.field.field_type.finalize(syntax.clone()).await, name: field.field.name },
        };
        if include_refs {
            field.field.field_type = FinalizedTypes::Reference(Box::new(field.field.field_type));
        }

        fields.push(field);
    }

    let return_type = if let Some(return_type) = function.return_type.as_mut() {
        Some(return_type.await?.finalize(syntax.clone()).await)
    } else {
        None
    };

    let codeless = CodelessFinalizedFunction {
        generics: finalize_generics(syntax, function.generics).await?,
        arguments: fields,
        return_type,
        data: function.data.clone(),
        token: function.token.clone()
    };

    return Ok((codeless, function.code));
}

pub async fn verify_function_code(process_manager: &TypesChecker, resolver: Box<dyn NameResolver>,
                             code: CodeBody,
                             codeless: CodelessFinalizedFunction, syntax: &Arc<Mutex<Syntax>>,
                             include_refs: bool) -> Result<FinalizedFunction, ParsingError> {
    {
        let mut locked = syntax.lock().unwrap();
        if let Some(wakers) = locked.functions.wakers.remove(&codeless.data.name) {
            for waker in wakers {
                waker.wake();
            }
        }

        locked.functions.data.insert(codeless.data.clone(), Arc::new(codeless.clone()));
    }

    //Internal/external/trait functions verify everything but the code.
    if is_modifier(codeless.data.modifiers, Modifier::Internal) || is_modifier(codeless.data.modifiers, Modifier::Extern) {
        return Ok(codeless.clone().add_code(FinalizedCodeBody::new(Vec::new(), String::new(), true)));
    }

    let mut variable_manager = SimpleVariableManager::for_function(&codeless);

    let mut code = verify_code(process_manager, &resolver, code, &codeless.return_type, syntax,
                               &mut variable_manager, include_refs, true).await?;

    if !code.returns {
        if codeless.return_type.is_none() {
            code.expressions.push(FinalizedExpression::new(ExpressionType::Return(CodeErrorToken::make_empty()), FinalizedEffects::NOP()));
        } else if !is_modifier(codeless.data.modifiers, Modifier::Trait) {
            return Err(codeless.token.make_error(format!("Function {} returns void instead of a {}!", codeless.data.name,
                                                 codeless.return_type.as_ref().unwrap())));
        }
    }

    return Ok(codeless.clone().add_code(code));
}
