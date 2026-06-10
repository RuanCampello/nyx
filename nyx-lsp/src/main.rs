use nyx_lsp::Lsp;
use tower_lsp::{LspService, Server};

#[tokio::main]
async fn main() {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = LspService::new(Lsp::new);

    Server::new(stdin, stdout, socket).serve(service).await;
}
