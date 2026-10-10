//! The Kiln language server (`kiln lsp`): diagnostics, hover, go to
//! definition, find references, rename, completion, document symbols,
//! formatting, and inlay type hints, over stdio.

pub mod analysis;
pub mod server;

#[cfg(test)]
mod tests;
