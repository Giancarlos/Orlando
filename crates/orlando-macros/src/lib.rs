use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Ident, ItemFn, ItemStruct, LitInt, LitStr, Path, Type};

// ── #[grain(state = T)] ────────────────────────────────────────

/// Define a grain type with its associated state.
///
/// ```ignore
/// #[grain(state = CounterState)]
/// struct Counter;
///
/// #[grain(state = CounterState, idle_timeout_secs = 60)]
/// struct Counter;
/// ```
#[proc_macro_attribute]
pub fn grain(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as GrainArgs);
    let item_struct = parse_macro_input!(item as ItemStruct);

    let name = &item_struct.ident;
    let state_type = &args.state;

    if args.stateless_worker && args.reentrant {
        return syn::Error::new(
            item_struct.ident.span(),
            "`stateless_worker` and `reentrant` are mutually exclusive — a stateless worker pool already provides concurrency",
        )
        .to_compile_error()
        .into();
    }

    let idle_timeout = args.idle_timeout_secs.map(|secs| {
        quote! {
            fn idle_timeout() -> ::std::time::Duration {
                ::std::time::Duration::from_secs(#secs)
            }
        }
    });

    let ask_timeout = args.ask_timeout_secs.map(|secs| {
        quote! {
            fn ask_timeout() -> ::std::time::Duration {
                ::std::time::Duration::from_secs(#secs)
            }
        }
    });

    let grain_type_name = args.grain_name.map(|n| {
        quote! {
            fn grain_type_name() -> &'static str { #n }
        }
    });

    let reentrant = if args.reentrant {
        quote! {
            fn reentrant() -> bool { true }
        }
    } else {
        quote! {}
    };

    let stateless_worker_impl = if args.stateless_worker {
        let max_act = args.max_activations.map(|n| {
            quote! {
                fn max_activations() -> usize { #n }
            }
        });
        quote! {
            impl ::orlando_core::StatelessWorker for #name {
                #max_act
            }
        }
    } else {
        quote! {}
    };

    quote! {
        #item_struct

        #[::async_trait::async_trait]
        impl ::orlando_core::Grain for #name {
            type State = #state_type;
            #idle_timeout
            #ask_timeout
            #grain_type_name
            #reentrant
        }

        #stateless_worker_impl
    }
    .into()
}

struct GrainArgs {
    state: Type,
    idle_timeout_secs: Option<u64>,
    stateless_worker: bool,
    max_activations: Option<usize>,
    reentrant: bool,
    grain_name: Option<String>,
    ask_timeout_secs: Option<u64>,
}

impl syn::parse::Parse for GrainArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut state = None;
        let mut idle_timeout_secs = None;
        let mut stateless_worker = false;
        let mut max_activations = None;
        let mut reentrant = false;
        let mut grain_name = None;
        let mut ask_timeout_secs = None;

        while !input.is_empty() {
            let key: Ident = input.parse()?;

            match key.to_string().as_str() {
                "state" => {
                    input.parse::<syn::Token![=]>()?;
                    state = Some(input.parse::<Type>()?);
                }
                "idle_timeout_secs" => {
                    input.parse::<syn::Token![=]>()?;
                    let lit: LitInt = input.parse()?;
                    idle_timeout_secs = Some(lit.base10_parse::<u64>()?);
                }
                "stateless_worker" => {
                    stateless_worker = true;
                }
                "max_activations" => {
                    input.parse::<syn::Token![=]>()?;
                    let lit: LitInt = input.parse()?;
                    max_activations = Some(lit.base10_parse::<usize>()?);
                }
                "reentrant" => {
                    reentrant = true;
                }
                "name" => {
                    input.parse::<syn::Token![=]>()?;
                    let lit: LitStr = input.parse()?;
                    grain_name = Some(lit.value());
                }
                "ask_timeout_secs" => {
                    input.parse::<syn::Token![=]>()?;
                    let lit: LitInt = input.parse()?;
                    ask_timeout_secs = Some(lit.base10_parse::<u64>()?);
                }
                _ => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown attribute `{}`", key),
                    ));
                }
            }

            if !input.is_empty() {
                input.parse::<syn::Token![,]>()?;
            }
        }

        let state = state.ok_or_else(|| input.error("missing required `state` attribute"))?;
        Ok(GrainArgs {
            state,
            idle_timeout_secs,
            stateless_worker,
            max_activations,
            reentrant,
            grain_name,
            ask_timeout_secs,
        })
    }
}

// ── #[message(result = T)] ─────────────────────────────────────

/// Define a message type with its result.
///
/// ```ignore
/// #[message(result = i64)]
/// struct GetCount;
///
/// // For cluster-capable messages (bincode only):
/// #[message(result = i64, network)]
/// #[derive(Serialize, Deserialize)]
/// struct GetCount;
///
/// // For cluster + external client support (bincode + protobuf):
/// #[message(result = CounterResult, network, proto)]
/// #[derive(Serialize, Deserialize, prost::Message)]
/// struct GetCount { ... }
/// ```
#[proc_macro_attribute]
pub fn message(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as MessageArgs);
    let item_struct = parse_macro_input!(item as ItemStruct);

    let name = &item_struct.ident;
    let result_type = &args.result;

    if args.proto && !args.network {
        return syn::Error::new(
            item_struct.ident.span(),
            "`proto` requires `network` — protobuf encoding is only available for network-capable messages",
        )
        .to_compile_error()
        .into();
    }

    let network_impl = if args.network {
        let name_str = name.to_string();

        let proto_methods = if args.proto {
            quote! {
                fn supports_proto() -> bool { true }

                fn encode_proto(&self) -> Option<Vec<u8>> {
                    use ::prost::Message;
                    Some(::prost::Message::encode_to_vec(self))
                }

                fn decode_proto(bytes: &[u8]) -> Option<Self> {
                    use ::prost::Message;
                    <Self as ::prost::Message>::decode(bytes).ok()
                }

                fn encode_result_proto(result: &<Self as ::orlando_core::Message>::Result) -> Option<Vec<u8>> {
                    use ::prost::Message;
                    Some(::prost::Message::encode_to_vec(result))
                }

                fn decode_result_proto(bytes: &[u8]) -> Option<<Self as ::orlando_core::Message>::Result> {
                    use ::prost::Message;
                    <Self as ::orlando_core::Message>::Result::decode(bytes).ok()
                }
            }
        } else {
            quote! {}
        };

        quote! {
            impl ::orlando_cluster::NetworkMessage for #name {
                fn message_type_name() -> &'static str {
                    #name_str
                }
                #proto_methods
            }
        }
    } else {
        quote! {}
    };

    quote! {
        #item_struct

        impl ::orlando_core::Message for #name {
            type Result = #result_type;
        }

        #network_impl
    }
    .into()
}

struct MessageArgs {
    result: Type,
    network: bool,
    proto: bool,
}

impl syn::parse::Parse for MessageArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut result = None;
        let mut network = false;
        let mut proto = false;

        while !input.is_empty() {
            let key: Ident = input.parse()?;

            match key.to_string().as_str() {
                "result" => {
                    input.parse::<syn::Token![=]>()?;
                    result = Some(input.parse::<Type>()?);
                }
                "network" => {
                    network = true;
                }
                "proto" => {
                    proto = true;
                }
                _ => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!("unknown attribute `{}`", key),
                    ));
                }
            }

            if !input.is_empty() {
                input.parse::<syn::Token![,]>()?;
            }
        }

        let result = result.ok_or_else(|| input.error("missing required `result` attribute"))?;
        Ok(MessageArgs {
            result,
            network,
            proto,
        })
    }
}

// ── #[grain_handler(GrainType)] ────────────────────────────────

/// Define a handler for a grain + message combination.
///
/// ```ignore
/// #[grain_handler(Counter)]
/// async fn handle(state: &mut CounterState, msg: Increment, _ctx: &GrainContext) -> i64 {
///     state.count += msg.amount;
///     state.count
/// }
/// ```
#[proc_macro_attribute]
pub fn grain_handler(attr: TokenStream, item: TokenStream) -> TokenStream {
    let grain_path = parse_macro_input!(attr as Path);
    let func = parse_macro_input!(item as ItemFn);

    let inputs = &func.sig.inputs;
    let output = &func.sig.output;
    let body = &func.block;
    let attrs = &func.attrs;

    let msg_type = match extract_msg_type(inputs) {
        Some(ty) => ty,
        None => {
            return syn::Error::new_spanned(
                &func.sig,
                "grain_handler requires at least 2 parameters: (state: &mut State, msg: M, ...)",
            )
            .to_compile_error()
            .into();
        }
    };

    quote! {
        #[::async_trait::async_trait]
        impl ::orlando_core::GrainHandler<#msg_type> for #grain_path {
            #(#attrs)*
            async fn handle(#inputs) #output
                #body
        }
    }
    .into()
}

fn extract_msg_type(
    inputs: &syn::punctuated::Punctuated<syn::FnArg, syn::Token![,]>,
) -> Option<Type> {
    let second = inputs.iter().nth(1)?;
    match second {
        syn::FnArg::Typed(pat_type) => Some((*pat_type.ty).clone()),
        _ => None,
    }
}
