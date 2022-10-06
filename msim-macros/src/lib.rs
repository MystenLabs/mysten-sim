//! Macros for use with Madsim

mod request;
mod service;

use proc_macro::TokenStream;
use proc_macro2::{Ident, Span};
use quote::{quote, quote_spanned, ToTokens};
use syn::DeriveInput;

#[proc_macro_derive(Request, attributes(rtype))]
pub fn message_derive_rtype(input: TokenStream) -> TokenStream {
    let ast: DeriveInput = syn::parse(input).unwrap();

    request::expand(&ast).into()
}

#[proc_macro_attribute]
pub fn service(args: TokenStream, input: TokenStream) -> TokenStream {
    service::service(args, input)
}

#[allow(clippy::needless_doctest_main)]
/// Marks async function to be executed by the selected runtime. This macro
/// helps set up a `Runtime` without requiring the user to use
/// [Runtime](../msim/runtime/struct.Runtime.html) directly.
///
/// # Example
///
/// ```ignore
/// #[msim::main]
/// async fn main() {
///     println!("Hello world");
/// }
/// ```
///
/// Equivalent code not using `#[msim::main]`
///
/// ```ignore
/// fn main() {
///     msim::runtime::Runtime::new().block_on(async {
///         println!("Hello world");
///     });
/// }
/// ```
#[proc_macro_attribute]
pub fn main(args: TokenStream, item: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(item as syn::ItemFn);
    let args = syn::parse_macro_input!(args as syn::AttributeArgs);

    parse_main(input, args).unwrap_or_else(|e| e.to_compile_error().into())
}

fn parse_main(
    mut input: syn::ItemFn,
    _args: syn::AttributeArgs,
) -> Result<TokenStream, syn::Error> {
    if input.sig.asyncness.take().is_none() {
        let msg = "the `async` keyword is missing from the function declaration";
        return Err(syn::Error::new_spanned(input.sig.fn_token, msg));
    }

    let body = &input.block;
    let brace_token = input.block.brace_token;
    input.block = syn::parse2(quote! {
        {
            let mut rt = ::msim::runtime::Runtime::new();
            rt.block_on(async #body)
        }
    })
    .expect("Parsing failure");
    input.block.brace_token = brace_token;

    let result = quote! {
        #input
    };

    Ok(result.into())
}

/// This macro will be exposed as #[tokio::test], so we want to mark it as ignored when running in
/// the simulator
#[proc_macro_attribute]
pub fn test(_args: TokenStream, item: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(item as syn::ItemFn);

    let fn_name = input.sig.ident.clone();
    let result = quote! {
        #[::core::prelude::v1::test]
        #[ignore = "tokio-only test"]
        fn #fn_name () {
            unimplemented!("this test can only be run in tokio");

            // paste original function to silence un-used import errors.
            #[allow(dead_code)]
            #input
        }
    };
    result.into()
}

/// Marks async function to be executed by runtime, suitable to test environment.
///
/// # Example
/// ```ignore
/// #[msim::test]
/// async fn my_test() {
///     assert!(true);
/// }
/// ```
///
/// # Configuration
///
/// Test can be configured using the following environment variables:
///
/// - `MSIM_TEST_SEED`: Set the random seed for test.
///
///     By default, the seed is set to the seconds since the Unix epoch.
///
/// - `MSIM_TEST_NUM`: Set the number of tests.
///
///     The seed will increase by 1 for each test.
///
///     By default, the number is 1.
///
/// - `MSIM_TEST_CONFIG`: Set the config file path.
///
///     By default, tests will use the default configuration.
///
/// - `MSIM_TEST_TIME_LIMIT`: Set the time limit for the test.
///
///     The test will panic if time limit exceeded in the simulation.
///
///     By default, there is no time limit.
///
/// - `MSIM_TEST_CHECK_DETERMINISM`: Enable determinism check.
///
///     The test will be run at least twice with the same seed.
///     If any non-determinism detected, it will panic as soon as possible.
///
///     By default, it is disabled.
///
/// The test can also be provided a configuration by passing an expression with a type that
/// can be made into() a TestConfig - SimConfig is the basic choice, see TestConfig for more
/// options.
///
/// Usage:
///
///    fn my_config() -> SimConfig {
///       SimConfig { ... }
///    }
///
///    #[sim_test(config = "my_config()")]
///    async fn test() {
///      ...
///    }
#[proc_macro_attribute]
pub fn sim_test(args: TokenStream, item: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(item as syn::ItemFn);
    let args = syn::parse_macro_input!(args as syn::AttributeArgs);

    parse_test(input, args).unwrap_or_else(|e| e.to_compile_error().into())
}

fn parse_test(mut input: syn::ItemFn, args: syn::AttributeArgs) -> Result<TokenStream, syn::Error> {
    if input.sig.asyncness.take().is_none() {
        let msg = "the `async` keyword is missing from the function declaration";
        return Err(syn::Error::new_spanned(input.sig.fn_token, msg));
    }

    let test_config = build_test_config(args)?;

    let body = &input.block;

    let (last_stmt_start_span, last_stmt_end_span) = {
        let mut last_stmt = input
            .block
            .stmts
            .last()
            .map(ToTokens::into_token_stream)
            .unwrap_or_default()
            .into_iter();
        // `Span` on stable Rust has a limitation that only points to the first
        // token, not the whole tokens. We can work around this limitation by
        // using the first/last span of the tokens like
        // `syn::Error::new_spanned` does.
        let start = last_stmt.next().map_or_else(Span::call_site, |t| t.span());
        let end = last_stmt.last().map_or(start, |t| t.span());
        (start, end)
    };
    let crate_name = test_config.crate_name.as_deref().unwrap_or("msim");
    let crate_ident = Ident::new(crate_name, last_stmt_start_span);

    let body: Box<syn::Block> = if test_config.run_in_client_node {
        syn::parse2(quote_spanned! {last_stmt_start_span=>
            {
                use std::str::FromStr;
                let ip = std::net::IpAddr::from_str("1.1.1.1").unwrap();
                let handle = #crate_ident::runtime::Handle::current();
                let builder = handle.create_node();
                let node = builder
                    .ip(ip)
                    .name("client")
                    .init(|| async {
                        ::tracing::info!("client restarted");
                    })
                    .build();

                let res = node.spawn(async move #body).await
                    .expect("join error in test runner");

                handle.kill(node.id());

                res
            }
        })
        .expect("Parsing failure")
    } else {
        body.clone()
    };

    let config_expr = test_config.network_config_expr.unwrap_or_else(|| {
        syn::parse2(quote! { #crate_ident::SimConfig::default() }).expect("parse error")
    });

    let check_determinism = test_config.check_determinism;

    let brace_token = input.block.brace_token;
    input.block = syn::parse2(quote_spanned! {last_stmt_end_span=>
        {
            let mut seed: u64 = if let Ok(seed_str) = ::std::env::var("MSIM_TEST_SEED") {
                seed_str.parse().expect("MSIM_TEST_SEED should be an integer")
            } else {
                ::std::time::SystemTime::now().duration_since(::std::time::SystemTime::UNIX_EPOCH).unwrap().as_secs()
            };
            let mut count: u64 = if let Ok(num_str) = std::env::var("MSIM_TEST_NUM") {
                num_str.parse().expect("MSIM_TEST_NUM should be an integer")
            } else {
                1
            };
            let time_limit_s = std::env::var("MSIM_TEST_TIME_LIMIT").ok().map(|num_str| {
                num_str.parse::<f64>().expect("MSIM_TEST_TIME_LIMIT should be an number")
            });
            let check = ::std::env::var("MSIM_TEST_CHECK_DETERMINISM").is_ok() || #check_determinism;
            if check {
                count = count.max(2);
            }

            fn next_seed(seed: u64) -> u64 {
                use rand::Rng;
                #crate_ident::rand::GlobalRng::new_with_seed(seed).gen::<u64>()
            }

            let mut rand_log = None;
            let mut return_value = None;
            for _i in 0..count {
                let mut inner_seed = seed;

                let config = std::thread::spawn(move || {
                    let rt = #crate_ident::runtime::Runtime::with_seed_and_config(inner_seed, #crate_ident::SimConfig::default());
                    rt.block_on(async move {
                        // run config_expr inside runtime so it can access rng.
                        #config_expr
                    })
                }).join().expect("config generation thread panicked!");

                let test_config: #crate_ident::TestConfig = config.into();
                if check {
                    assert_eq!(
                        test_config.configs.len(), 1,
                        "can't check determinism with repeated test"
                    );
                }

                for (repeat, sim_config) in test_config.configs.iter() {
                    assert_ne!(*repeat, 0);
                    if check {
                        assert_eq!(
                            *repeat, 1,
                            "can't check determinism with repeated test"
                        );
                    }

                    for _j in 0..*repeat {
                        let sim_config = sim_config.clone();
                        let rand_log0 = rand_log.take();
                        let res = std::thread::spawn(move || {
                            let mut rt = #crate_ident::runtime::Runtime::with_seed_and_config(inner_seed, sim_config);
                            if check {
                                rt.enable_determinism_check(rand_log0);
                            }
                            if let Some(limit) = time_limit_s {
                                rt.set_time_limit(::std::time::Duration::from_secs_f64(limit));
                            }
                            let ret = rt.block_on(async #body);
                            let log = rt.take_rand_log();
                            (ret, log)
                        }).join();
                        match res {
                            Ok((ret, log)) => {
                                return_value = Some(ret);
                                rand_log = log;
                            }
                            Err(e) => {
                                println!("note: run with `MSIM_TEST_SEED={}` environment variable to reproduce this error", inner_seed);
                                ::std::panic::resume_unwind(e);
                            }
                        }
                        inner_seed += 1;
                    }
                }

                if !check {
                    seed = next_seed(seed);
                }
            }
            return_value.unwrap()
        }
    })
    .expect("Parsing failure");
    input.block.brace_token = brace_token;

    let result = quote! {
        #[::core::prelude::v1::test]
        #input
    };

    Ok(result.into())
}

struct TestConfig {
    crate_name: Option<String>,
    network_config_expr: Option<syn::Expr>,
    run_in_client_node: bool,
    check_determinism: bool,
}

impl TestConfig {
    fn set_crate_name(&mut self, name: syn::Lit, span: Span) -> Result<(), syn::Error> {
        if self.crate_name.is_some() {
            return Err(syn::Error::new(span, "`crate` set multiple times."));
        }
        let name_ident = parse_ident(name, span, "crate")?;
        self.crate_name = Some(name_ident.to_string());
        Ok(())
    }

    fn set_config_network_expr(&mut self, expr: syn::Expr) {
        self.network_config_expr = Some(expr);
    }
}

impl Default for TestConfig {
    fn default() -> Self {
        Self {
            crate_name: None,
            network_config_expr: None,
            run_in_client_node: true,
            check_determinism: false,
        }
    }
}

fn build_test_config(args: syn::AttributeArgs) -> Result<TestConfig, syn::Error> {
    let mut config: TestConfig = Default::default();

    // no need to support tokio::main in simulator
    let macro_name = "test";

    for arg in args {
        match arg {
            syn::NestedMeta::Meta(syn::Meta::NameValue(namevalue)) => {
                let ident = namevalue
                    .path
                    .get_ident()
                    .ok_or_else(|| {
                        syn::Error::new_spanned(&namevalue, "Must have specified ident")
                    })?
                    .to_string()
                    .to_lowercase();
                match ident.as_str() {
                    "worker_threads" => {
                        println!("simulator: ignoring `worker_threads` setting");
                    }
                    "flavor" => {
                        println!("simulator: ignoring `flavor` setting");
                    }
                    "start_paused" => {
                        println!("simulator: ignoring `start_paused` setting");
                    }
                    "no_client_node" => {
                        config.run_in_client_node = false;
                    }
                    "config" => {
                        let expr = match &namevalue.lit {
                            syn::Lit::Str(litstr) => syn::parse_str::<syn::Expr>(&litstr.value())?,
                            _ => {
                                let msg = format!("expected string literal");
                                return Err(syn::Error::new_spanned(namevalue, msg));
                            }
                        };
                        config.set_config_network_expr(expr);
                    }
                    "crate" => {
                        config.set_crate_name(
                            namevalue.lit.clone(),
                            syn::spanned::Spanned::span(&namevalue.lit),
                        )?;
                    }
                    name => {
                        let msg = format!(
                            "Unknown attribute {} is specified; expected one of: `flavor`, `worker_threads`, `start_paused`, `crate`",
                            name,
                        );
                        return Err(syn::Error::new_spanned(namevalue, msg));
                    }
                }
            }
            syn::NestedMeta::Meta(syn::Meta::Path(path)) => {
                let name = path
                    .get_ident()
                    .ok_or_else(|| syn::Error::new_spanned(&path, "Must have specified ident"))?
                    .to_string()
                    .to_lowercase();
                let msg = match name.as_str() {
                    "check_determinism" => {
                        config.check_determinism = true;
                        continue;
                    }
                    "threaded_scheduler" | "multi_thread" => {
                        format!(
                            "Set the runtime flavor with #[{}(flavor = \"multi_thread\")].",
                            macro_name
                        )
                    }
                    "basic_scheduler" | "current_thread" | "single_threaded" => {
                        format!(
                            "Set the runtime flavor with #[{}(flavor = \"current_thread\")].",
                            macro_name
                        )
                    }
                    "flavor" | "worker_threads" | "start_paused" => {
                        format!("The `{}` attribute requires an argument.", name)
                    }
                    name => {
                        format!("Unknown attribute {} is specified; expected one of: `flavor`, `worker_threads`, `start_paused`, `crate`, `check_determinism`", name)
                    }
                };
                return Err(syn::Error::new_spanned(path, msg));
            }
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "Unknown attribute inside the macro",
                ));
            }
        }
    }

    Ok(config)
}

fn parse_ident(lit: syn::Lit, span: Span, field: &str) -> Result<Ident, syn::Error> {
    match lit {
        syn::Lit::Str(s) => {
            let err = syn::Error::new(
                span,
                format!(
                    "Failed to parse value of `{}` as ident: \"{}\"",
                    field,
                    s.value()
                ),
            );
            let path = s.parse::<syn::Path>().map_err(|_| err.clone())?;
            path.get_ident().cloned().ok_or(err)
        }
        _ => Err(syn::Error::new(
            span,
            format!("Failed to parse value of `{}` as ident.", field),
        )),
    }
}
