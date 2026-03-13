mod crate_index;
mod feature_index;
mod lsp;
mod toml_context;
mod toml_context_fallback;

use tower_lsp::{LspService, Server};

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(lsp::Backend::new);
    Server::new(stdin, stdout, socket).serve(service).await;
}
