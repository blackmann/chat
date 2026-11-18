//! Language servers, for finding your way around code under review: where a
//! symbol is defined, where else it's used, what it is. The panes are read-only,
//! so servers are only told what's on disk.
mod client;
pub mod install;
pub mod position;
mod registry;
mod server;

pub use client::{Client, Incoming};
pub use registry::{ServerSpec, language_id, root_for};
#[cfg(test)]
pub use registry::proxy_missing;
pub use registry::which;
pub use registry::{path, unique, uri};
pub use server::Encoding;
pub use server::{Server, Status, initialize_params, install_hint};

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use lsp_types::{
        DocumentHighlightParams, GotoDefinitionParams, GotoDefinitionResponse, HoverParams, Position, ReferenceContext, ReferenceParams,
        TextDocumentIdentifier, TextDocumentPositionParams,
        request::{DocumentHighlightRequest, GotoDefinition, HoverRequest, Initialize, References},
    };

    /// A real round trip: start clangd, open a file, ask where a call goes.
    #[test]
    fn finds_a_definition_with_clangd() {
        let Some(clangd) = which("clangd", None) else { return };
        let dir = std::env::temp_dir().join(format!("chat-lsp-clangd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.c");
        let text = "int twice(int x) { return x * 2; }\nint main(void) { return twice(21); }\n";
        std::fs::write(&file, text).unwrap();

        let spec = ServerSpec::defaults().into_iter().find(|s| s.name == "clangd").unwrap();
        let (client, _incoming) = Client::spawn(clangd.to_str().unwrap(), &spec.args, &dir, None).unwrap();
        let mut server = Server::new(spec.clone(), dir.clone(), Status::Starting, Some(client.clone()));
        server.open(&file, "c", text.into());
        let result = block_on(client.call::<Initialize>(initialize_params(&spec, &dir))).unwrap();
        server.ready(result);
        assert_eq!(server.status, Status::Ready);

        // `twice` in `main`, on the second line.
        let at = TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri: uri(&file).unwrap() },
            position: Position::new(1, 26),
        };
        let params = GotoDefinitionParams { text_document_position_params: at, work_done_progress_params: Default::default(), partial_result_params: Default::default() };
        let found = block_on(client.call::<GotoDefinition>(params)).unwrap().expect("a definition");
        let first = match found {
            GotoDefinitionResponse::Scalar(l) => (l.uri, l.range),
            GotoDefinitionResponse::Array(ls) => (ls[0].uri.clone(), ls[0].range),
            GotoDefinitionResponse::Link(ls) => (ls[0].target_uri.clone(), ls[0].target_selection_range),
        };
        assert_eq!(path(&first.0).unwrap().canonicalize().unwrap(), file.canonicalize().unwrap());
        assert_eq!(first.1.start, Position::new(0, 4));

        // The other requests the panes make, at the same place.
        let doc = || TextDocumentIdentifier { uri: uri(&file).unwrap() };
        let at = || TextDocumentPositionParams { text_document: doc(), position: Position::new(1, 26) };
        let highlights = block_on(client.call::<DocumentHighlightRequest>(DocumentHighlightParams {
            text_document_position_params: at(),
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }))
        .unwrap()
        .unwrap_or_default();
        // Only the call is in this file's `main`; the definition is too.
        let lines: Vec<u32> = highlights.iter().map(|h| h.range.start.line).collect();
        assert!(lines.contains(&0) && lines.contains(&1), "{lines:?}");

        let references = block_on(client.call::<References>(ReferenceParams {
            text_document_position: at(),
            context: ReferenceContext { include_declaration: true },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }))
        .unwrap()
        .unwrap_or_default();
        // The temporary folder is reached through a symlink, so clangd names the
        // file both ways; the list shows each place once.
        let places = unique(references.into_iter().map(|l| (path(&l.uri).unwrap(), l.range)).collect());
        assert_eq!(places.len(), 2, "{places:?}");

        let hover = block_on(client.call::<HoverRequest>(HoverParams { text_document_position_params: at(), work_done_progress_params: Default::default() }))
            .unwrap()
            .expect("hover");
        assert!(serde_json::to_string(&hover.contents).unwrap().contains("twice"));

        // clangd counts in UTF-8 when asked.
        assert_eq!(server.encoding, Encoding::Utf8);
        client.shutdown();
        std::fs::remove_dir_all(&dir).ok();
    }
}
