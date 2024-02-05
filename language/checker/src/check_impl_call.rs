use std::mem;

use data::tokens::Span;
use syntax::async_util::{AsyncDataGetter, UnparsedType};
use syntax::code::{EffectType, Effects, FinalizedEffectType, FinalizedEffects};
use syntax::r#struct::VOID;
use syntax::syntax::Syntax;
use syntax::top_element_manager::ImplWaiter;
use syntax::types::FinalizedTypes;
use syntax::ProcessManager;
use syntax::{ParsingError, SimpleVariableManager};

use crate::check_code::verify_effect;
use crate::check_method_call::check_method;
use crate::degeneric::degeneric_header;
use crate::{get_return, CodeVerifier};

/// Checks an implementation call generated by control_parser or an operator to get the correct method
pub async fn check_impl_call(
    code_verifier: &mut CodeVerifier<'_>,
    variables: &mut SimpleVariableManager,
    effect: Effects,
) -> Result<FinalizedEffects, ParsingError> {
    let mut finalized_effects = Vec::default();
    let calling;
    let traits;
    let method;
    let returning;
    if let EffectType::ImplementationCall(new_calling, new_traits, new_method, effects, new_returning) = effect.types {
        for effect in effects {
            finalized_effects.push(verify_effect(code_verifier, variables, effect).await?)
        }
        calling = new_calling;
        traits = new_traits;
        method = new_method;
        returning = new_returning;
    } else {
        unreachable!()
    }

    let mut finding_return_type;
    if matches!(calling.types, EffectType::NOP) {
        finding_return_type = FinalizedTypes::Struct(VOID.clone());
    } else {
        let found = verify_effect(code_verifier, variables, *calling).await?;
        finding_return_type = get_return(&found.types, variables, &code_verifier.syntax).await.unwrap();
        finding_return_type.fix_generics(code_verifier.process_manager, &code_verifier.syntax).await?;
        finalized_effects.insert(0, found);
    }

    if let Ok(inner) = Syntax::get_struct(
        code_verifier.syntax.clone(),
        ParsingError::new(Span::default(), "You shouldn't see this! Report this please! Location: Check impl call"),
        traits.clone(),
        code_verifier.resolver.boxed_clone(),
        vec![],
    )
    .await
    {
        let data = inner.finalize(code_verifier.syntax.clone()).await;

        let mut impl_checker = ImplCheckerData {
            code_verifier,
            data: &data,
            returning: &returning,
            method: &method,
            finding_return_type: &finding_return_type,
            finalized_effects: &mut finalized_effects,
            variables,
        };
        if let Some(found) = check_virtual_type(&mut impl_checker, &effect.span).await? {
            return Ok(found);
        }

        let mut output = None;
        while output.is_none() && !code_verifier.syntax.lock().unwrap().finished_impls() {
            output = try_get_impl(&impl_checker, &effect.span).await?;
        }

        if output.is_none() {
            output = try_get_impl(&impl_checker, &effect.span).await?;
        }

        if output.is_none() {
            panic!("Failed for {} and {}", finding_return_type, data);
        }
        return Ok(output.unwrap());
    } else {
        panic!("Screwed up trait! {} for {:?}", traits, code_verifier.resolver.imports());
    }
}

/// All the data used by implementation checkers
pub struct ImplCheckerData<'a> {
    /// The code verified fields
    code_verifier: &'a CodeVerifier<'a>,
    /// Type being checked
    data: &'a FinalizedTypes,
    /// The generic return type
    returning: &'a Option<UnparsedType>,
    /// The name of the method, can be empty to just return the first found method
    method: &'a String,
    /// The trait to find
    finding_return_type: &'a FinalizedTypes,
    /// The arguments
    finalized_effects: &'a mut Vec<FinalizedEffects>,
    /// The current variables
    variables: &'a SimpleVariableManager,
}

/// Checks an implementation call to see if it should be a virtual call
async fn check_virtual_type(data: &mut ImplCheckerData<'_>, token: &Span) -> Result<Option<FinalizedEffects>, ParsingError> {
    if data.finding_return_type.of_type_sync(data.data, None).0 {
        let mut i = 0;
        for found in &data.data.inner_struct().data.functions {
            if found.name == *data.method {
                let mut temp = vec![];
                mem::swap(&mut temp, data.finalized_effects);
                let function = AsyncDataGetter::new(data.code_verifier.syntax.clone(), found.clone()).await;

                return Ok(Some(FinalizedEffects::new(token.clone(), FinalizedEffectType::VirtualCall(i, function, temp))));
            } else if found.name.split("::").last().unwrap() == data.method {
                let mut target = data.finding_return_type.find_method(&data.method).unwrap();
                if target.len() > 1 {
                    return Err(token.make_error("Ambiguous function!"));
                } else if target.is_empty() {
                    return Err(token.make_error("Unknown function!"));
                }
                let (_, target) = target.pop().unwrap();

                let return_type =
                    get_return(&data.finalized_effects[0].types, data.variables, &data.code_verifier.syntax).await.unwrap();
                if matches!(return_type, FinalizedTypes::Generic(_, _)) {
                    let mut temp = vec![];
                    mem::swap(&mut temp, data.finalized_effects);
                    return Ok(Some(FinalizedEffects::new(
                        token.clone(),
                        FinalizedEffectType::GenericVirtualCall(
                            i,
                            target,
                            AsyncDataGetter::new(data.code_verifier.syntax.clone(), found.clone()).await,
                            temp,
                        ),
                    )));
                }

                data.code_verifier.syntax.lock().unwrap().process_manager.handle().lock().unwrap().spawn(
                    target.name.clone(),
                    degeneric_header(
                        target.clone(),
                        found.clone(),
                        data.code_verifier.syntax.clone(),
                        data.code_verifier.process_manager.cloned(),
                        data.finalized_effects.clone(),
                        data.variables.clone(),
                        token.clone(),
                    ),
                );

                let output = AsyncDataGetter::new(data.code_verifier.syntax.clone(), target.clone()).await;
                let mut temp = vec![];
                mem::swap(&mut temp, data.finalized_effects);
                return Ok(Some(FinalizedEffects::new(token.clone(), FinalizedEffectType::VirtualCall(i, output, temp))));
            }
            i += 1;
        }

        if !data.method.is_empty() {
            return Err(token.make_error("Unknown method!"));
        }
    }
    return Ok(None);
}

/// Tries to get an implementation matching the types passed in
async fn try_get_impl(data: &ImplCheckerData<'_>, span: &Span) -> Result<Option<FinalizedEffects>, ParsingError> {
    let result = ImplWaiter {
        syntax: data.code_verifier.syntax.clone(),
        // [Dependency]
        return_type: data.finding_return_type.clone(),
        // CreateArray
        data: data.data.clone(),
        error: span.make_error("Nothing implements the given trait!"),
    }
    .await?;

    for temp in &result {
        if temp.name.split("::").last().unwrap() == data.method || data.method.is_empty() {
            let method = AsyncDataGetter::new(data.code_verifier.syntax.clone(), temp.clone()).await;

            let returning = match &data.returning {
                Some(inner) => Some((
                    Syntax::parse_type(
                        data.code_verifier.syntax.clone(),
                        span.make_error("Incorrect bounds!"),
                        data.code_verifier.resolver.boxed_clone(),
                        inner.clone(),
                        vec![],
                    )
                    .await?
                    .finalize(data.code_verifier.syntax.clone())
                    .await,
                    span.clone(),
                )),
                None => None,
            };

            match check_method(
                &data.code_verifier.process_manager,
                method.clone(),
                data.finalized_effects.clone(),
                &data.code_verifier.syntax,
                &data.variables,
                returning,
                span,
            )
            .await
            {
                Ok(found) => return Ok(Some(found)),
                Err(_error) => {}
            };
        }
    }
    return Ok(None);
}
