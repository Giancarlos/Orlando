use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Ident, ItemFn, ItemStruct, LitInt, Path, Type};

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

    let idle_timeout = args.idle_timeout_secs.map(|secs| {
        quote! {
            fn idle_timeout() -> ::std::time::Duration {
                ::std::time::Duration::from_secs(#secs)
            }
        }
    });

    quote! {
        #item_struct

        #[::async_trait::async_trait]
        impl ::orlando_core::Grain for #name {
            type State = #state_type;
            #idle_timeout
        }
    }
    .into()
}

struct GrainArgs {
    state: Type,
    idle_timeout_secs: Option<u64>,
}

impl syn::parse::Parse for GrainArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut state = None;
        let mut idle_timeout_secs = None;

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<syn::Token![=]>()?;

            match key.to_string().as_str() {
                "state" => {
                    state = Some(input.parse::<Type>()?);
                }
                "idle_timeout_secs" => {
                    let lit: LitInt = input.parse()?;
                    idle_timeout_secs = Some(lit.base10_parse::<u64>()?);
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
/// // For cluster-capable messages:
/// #[message(result = i64, network)]
/// #[derive(Serialize, Deserialize)]
/// struct GetCount;
/// ```
#[proc_macro_attribute]
pub fn message(attr: TokenStream, item: TokenStream) -> TokenStream {
    let args = parse_macro_input!(attr as MessageArgs);
    let item_struct = parse_macro_input!(item as ItemStruct);

    let name = &item_struct.ident;
    let result_type = &args.result;

    let network_impl = if args.network {
        let name_str = name.to_string();
        quote! {
            impl ::orlando_cluster::NetworkMessage for #name {
                fn message_type_name() -> &'static str {
                    #name_str
                }
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
}

impl syn::parse::Parse for MessageArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut result = None;
        let mut network = false;

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
        Ok(MessageArgs { result, network })
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
