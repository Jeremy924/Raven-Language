use std::sync::{Arc, Mutex};
use async_trait::async_trait;
use syntax::ParsingError;
use syntax::syntax::Syntax;

#[async_trait]
pub trait Compiler<Args, Output>: Send + Sync {
    /// Compiles the main function and returns the main runner.
    fn compile(&self, syntax: &Arc<Mutex<Syntax>>) -> Result<UnsafeFn<Args, Output>, Vec<ParsingError>>;
}

pub type UnsafeFn<Args, Output> = unsafe extern "C" fn(Args) -> Output;