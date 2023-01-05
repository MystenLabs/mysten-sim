#[cfg(test)]
mod test {
    use anemo::{types::PeerEvent, Network, NetworkRef, Request, Response, Result};
    use bytes::Bytes;
    use rand::RngCore;
    use std::convert::Infallible;
    use tokio::time::{sleep, Duration};
    use tower::{util::BoxCloneService, ServiceExt};
    use tracing::debug;

    use msim_macros::sim_test;

    fn build_network() -> Result<Network> {
        build_network_with_addr("127.0.0.1:0")
    }

    fn build_network_with_addr(addr: &str) -> Result<Network> {
        let mut rng = rand::thread_rng();
        let mut private_key = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rng, &mut private_key[..]);

        let network = Network::bind(addr)
            .private_key(private_key)
            .server_name("test")
            .start(echo_service())?;

        debug!(
            address =% network.local_addr(),
            peer_id =% network.peer_id(),
            "starting network"
        );

        Ok(network)
    }

    fn echo_service() -> BoxCloneService<Request<Bytes>, Response<Bytes>, Infallible> {
        let handle = move |request: Request<Bytes>| async move {
            debug!("recieved: {}", request.body().escape_ascii());
            let response = Response::new(request.into_body());
            Result::<Response<Bytes>, Infallible>::Ok(response)
        };

        tower::service_fn(handle).boxed_clone()
    }

    #[sim_test]
    async fn test() {
        {
            msim::runtime::init_logger();

            let msg = b"The Way of Kings";

            let network_1 = build_network().unwrap();
            let network_2 = build_network().unwrap();

            sleep(Duration::from_secs(1)).await;

            dbg!(network_1.local_addr());
            let peer = network_1
                .connect(dbg!(network_2.local_addr()))
                .await
                .unwrap();
            let response = network_1
                .rpc(peer, Request::new(msg.as_ref().into()))
                .await
                .unwrap();
            assert_eq!(response.into_body(), msg.as_ref());

            let msg = b"Words of Radiance";
            let peer_id_1 = network_1.peer_id();
            let response = network_2
                .rpc(peer_id_1, Request::new(msg.as_ref().into()))
                .await
                .unwrap();
            assert_eq!(response.into_body(), msg.as_ref());
        }
        tracing::warn!("=============================");
    }
}
