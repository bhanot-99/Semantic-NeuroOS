// knowledge query engine entry point [C5a]
use neuroos_knowledge_query::distill::DistillationCache;
use neuroos_knowledge_query::inference_client::InferenceClient;
use neuroos_knowledge_query::kernel_client::KernelClient;
use neuroos_knowledge_query::server::{self, Clients};
use neuroos_knowledge_query::storage_client::StorageClient;
use neuroos_knowledge_query::voice_client::VoiceClient;

fn current_uid() -> u32 {
    // SAFETY: getuid() takes no arguments and cannot fail.
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    unsafe { getuid() }
}

#[tokio::main]
async fn main() {
    if neuroos_common::init_logging().is_err() {
        eprintln!("neuroos-knowledge-query: logging already initialized (unexpected)");
    }

    let my_uid = current_uid();

    // P0-S04 convention: a health endpoint is one line.
    let health = neuroos_health::HealthServer::new(concat!(
        "neuroos-knowledge-query v",
        env!("CARGO_PKG_VERSION")
    ));
    tokio::spawn(health.serve(
        neuroos_common::paths::component_health_sock("neuroos-knowledge-query"),
        vec![my_uid],
    ));

    // AB-1: C5a never links neuroos-storage/neuroos-inference directly --
    // every downstream call goes over its own socket, same as every other
    // real client of C3/C4 (matches the neuroosctl<->healthd precedent).
    let clients = Clients {
        voice: VoiceClient::new(neuroos_common::paths::voice_sock()),
        storage: StorageClient::new(neuroos_common::paths::storage_sock()),
        inference: InferenceClient::new(neuroos_common::paths::inference_sock()),
        kernel: KernelClient::new(neuroos_common::paths::kernel_sock()),
        distill_cache: DistillationCache::new(),
    };

    server::serve(
        clients,
        neuroos_common::paths::knowledge_sock(),
        vec![my_uid],
    )
    .await;
}
