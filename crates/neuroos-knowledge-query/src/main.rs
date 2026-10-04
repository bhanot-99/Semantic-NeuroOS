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

fn main() {
    if neuroos_common::init_logging().is_err() {
        eprintln!("neuroos-knowledge-query: logging already initialized (unexpected)");
    }

    // H15: locked in before the async runtime spawns its worker threads.
    // The graph view file is created first so the sandbox can grant
    // exactly that one file in the data dir. Fail closed (rules.md §5.5).
    let graph_view = neuroos_common::paths::graph_view_html_file();
    let created = graph_view
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&graph_view)
                .map(drop)
        });
    if let Err(e) = created {
        tracing::error!(error = %e, "failed to create graph_view.html");
        std::process::exit(1);
    }
    let policy = neuroos_knowledge_query::sandbox_policy();
    if let Err(e) = neuroos_sandbox::enter(&policy, &[neuroos_common::paths::runtime_dir()]) {
        tracing::error!(error = %e, "failed to enter the Landlock sandbox");
        std::process::exit(1);
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "failed to start the async runtime");
            std::process::exit(1);
        }
    };
    runtime.block_on(run());
}

async fn run() {
    let my_uid = current_uid();

    // P0-S04 convention: a health endpoint is one line.
    let health = neuroos_health::HealthServer::new(concat!(
        "neuroos-knowledge-query v",
        env!("CARGO_PKG_VERSION")
    ));
    tokio::spawn(health.clone().serve(
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
        health,
    };

    server::serve(
        clients,
        neuroos_common::paths::knowledge_sock(),
        vec![my_uid],
    )
    .await;
}
