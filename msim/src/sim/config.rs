//! Simulation configuration.

pub use crate::net::config::*;

/// Simulation configuration.
#[cfg_attr(docsrs, doc(cfg(msim)))]
#[derive(Debug, Default, Clone)]
pub struct SimConfig {
    /// Network configurations.
    pub net: NetworkConfig,
}

/// Configuration for a series of tests
#[derive(Clone)]
pub struct TestConfig {
    /// The test will be repeated N times with each config
    pub configs: Vec<(usize, SimConfig)>,
}

impl From<SimConfig> for TestConfig {
    fn from(config: SimConfig) -> Self {
        Self {
            configs: vec![(1, config)],
        }
    }
}

impl From<Vec<SimConfig>> for TestConfig {
    fn from(config: Vec<SimConfig>) -> Self {
        Self {
            configs: config.iter().map(|c| (1, c.clone())).collect(),
        }
    }
}

impl From<Vec<(usize, SimConfig)>> for TestConfig {
    fn from(configs: Vec<(usize, SimConfig)>) -> Self {
        Self { configs }
    }
}

#[cfg(test)]
mod test {

    use super::*;
    use msim_macros::sim_test;
    use rand::Rng;

    fn test_config() -> SimConfig {
        Default::default()
    }

    fn test_config_multiple() -> Vec<SimConfig> {
        vec![Default::default(), Default::default()]
    }

    fn test_config_multiple_repeat() -> Vec<(usize, SimConfig)> {
        vec![(3, Default::default()), (3, Default::default())]
    }

    // There's no easy way to actually test the behavior here, so just run the test with
    // --no-capture and eyeball the output. (Main thing is you shouldn't be seeing repeated
    // output).

    #[sim_test(crate = "crate", config = "test_config()")]
    async fn config_test() {
        println!("single {:08x}", rand::thread_rng().r#gen::<u32>());
    }

    #[sim_test(crate = "crate", config = "test_config_multiple()")]
    async fn config_test_multiple() {
        println!("multiple {:08x}", rand::thread_rng().r#gen::<u32>());
    }

    #[sim_test(crate = "crate", config = "test_config_multiple_repeat()")]
    async fn config_test_multiple_repeat() {
        println!("multiple repeat {:08x}", rand::thread_rng().r#gen::<u32>());
    }
}
