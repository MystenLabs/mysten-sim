#[cfg(test)]
mod test {
    use std::net::SocketAddr;

    use jsonrpsee::core::client::ClientT;
    use jsonrpsee::http_client::HttpClientBuilder;
    use jsonrpsee::rpc_params;
    use jsonrpsee::server::{RpcModule, ServerBuilder};
    use jsonrpsee::ws_client::WsClientBuilder;

    use msim_macros::sim_test;

    pub async fn run_server() -> anyhow::Result<SocketAddr> {
        let server = ServerBuilder::default()
            .build("10.1.1.1:80".parse::<SocketAddr>()?)
            .await?;
        let mut module = RpcModule::new(());
        module.register_method("say_hello", |_, _| Ok("lo"))?;

        let addr = server.local_addr()?;
        let handle = server.start(module)?;

        // In this example we don't care about doing shutdown so let's it run forever.
        // You may use the `ServerHandle` to shut it down or manage it yourself.
        tokio::spawn(handle.stopped());

        Ok(addr)
    }

    #[sim_test]
    async fn test() {
        msim::runtime::init_logger();

        let ip = std::net::IpAddr::from_str("10.1.1.1").unwrap();
        let handle = msim::runtime::Handle::current();
        let builder = handle.create_node();
        let node = builder
            .ip(ip)
            .name("server")
            .init(|| async {
                println!("server restarted");
            })
            .build();

        let (tx, rx) = tokio::sync::oneshot::channel();

        node.spawn(async move {
            let server_addr = run_server().await.unwrap();
            tx.send(server_addr).unwrap();
        });

        let server_addr = rx.await.unwrap();

        let url = format!("http://{}", server_addr);

        let client = HttpClientBuilder::default().build(url).unwrap();
        let params = rpc_params![1_u64, 2, 3];
        let response: String = client.request("say_hello", params).await.unwrap();
        println!("http response: {:?}", response);

        let url = format!("ws://{}", server_addr);
        let ws_client = WsClientBuilder::default().build(&url).await.unwrap();
        let params = rpc_params![1_u64, 2, 3];
        let response: String = ws_client.request("say_hello", params).await.unwrap();
        println!("ws response: {:?}", response);

        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }
}
