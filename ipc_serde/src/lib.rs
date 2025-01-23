use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Expr, Fields, Meta};

/// Derive macro for IPC serialization — a custom serialization format for IPC messages in
/// Bitcoin transactions.
///
/// Uses serde_json to serialize and deserialize individual fields.
/// Differrent rules for serialization can be defined for different types.
//
// TODO Make a general way to handle different types of serialization
#[proc_macro_derive(IpcSerialize, attributes(tag, ipc_serde))]
pub fn ipc_serialize_derive(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let tag = input
        .attrs
        .iter()
        .find(|&attr| attr.path().is_ident("tag"))
        .expect("IPC Tag attribute missing")
        .parse_args::<Expr>()
        .expect("Tag attribute should be an expression");

    let fields = if let Data::Struct(data_struct) = &input.data {
        if let Fields::Named(fields_named) = &data_struct.fields {
            &fields_named.named
        } else {
            panic!("Expected named fields");
        }
    } else {
        panic!("Expected a struct");
    };

    /// Checks if the type should be (de)serialized without quotes
    // TODO make this automatic or more general
    fn without_quotes(ty: &syn::Type) -> bool {
        let string_types = [
            syn::parse_quote!(SubnetId),
            syn::parse_quote!(String),
            syn::parse_quote!(SocketAddr),
            syn::parse_quote!(std::net::SocketAddr),
            syn::parse_quote!(XOnlyPublicKey),
            syn::parse_quote!(bitcoin::XOnlyPublicKey),
            syn::parse_quote!(Address<NetworkUnchecked>),
            syn::parse_quote!(bitcoin::Address<NetworkUnchecked>),
            // Add other types that don't need quotes here
        ];
        string_types.contains(ty)
    }

    /// Checks if a field should be skipped
    fn should_skip_field(field: &syn::Field) -> bool {
        field.attrs.iter().any(|attr| {
            if attr.path().is_ident("ipc_serde") {
                if let Meta::List(list) = attr.meta.clone() {
                    return list.tokens.to_string().contains("skip");
                }
            }
            false
        })
    }

    let serialize_fields = fields.iter().filter_map(|field| {
    	if should_skip_field(field) {
            return None;
        }

        let field_name = &field.ident;
        let field_name_str = field_name.as_ref().unwrap().to_string();

        // Handle Vec<String>
        Some(if &field.ty == &syn::parse_quote!(Vec<String>) {
            quote! {
                params_map.insert(#field_name_str, self.#field_name.join(","));
            }

        // Handle Vec<XOnlyPublicKey>
        } else if &field.ty == &syn::parse_quote!(Vec<XOnlyPublicKey>) {
            quote! {
                params_map.insert(#field_name_str, self.#field_name.iter().map(|key| key.to_string()).collect::<Vec<_>>().join(","));
            }

        } else if without_quotes(&field.ty) {
			quote! {
				params_map.insert(#field_name_str, serde_json::to_string(&self.#field_name).unwrap().trim_matches('"')
					.to_string());
			}

        // Handle other json-serializable types
        } else {
		    quote! {
		        params_map.insert(#field_name_str, serde_json::to_string(&self.#field_name).unwrap());
		    }
        })
    });

    let deserialize_fields = fields.iter().filter_map(|field| {
         let field_name = &field.ident;

         if should_skip_field(field) {
			return Some(quote! {
				#field_name: Default::default(),
			});
		}

         let field_name_str = field_name.as_ref().unwrap().to_string();

         // Handle Vec<String>
         Some(if &field.ty == &syn::parse_quote!(Vec<String>) {
            quote! {
	            #field_name: params_map.remove(#field_name_str)
	                .ok_or_else(|| MissingField(#field_name_str.to_string()))?
	                .split(',')
	                .map(|s| s.parse().map_err(|e| ParseFieldError(#field_name_str.to_string(), e.to_string())))
	                .collect::<Result<_, _>>()?,
            }
        // Handle Vec<XOnlyPublicKey>
        } else if &field.ty == &syn::parse_quote!(Vec<XOnlyPublicKey>) {
			quote! {
	            #field_name: params_map.remove(#field_name_str)
	                .ok_or_else(|| MissingField(#field_name_str.to_string()))?
	                .split(',')
	                .map(|s| bitcoin::XOnlyPublicKey::from_str(s).map_err(|e| ParseFieldError(#field_name_str.to_string(), e.to_string())))
	                .collect::<Result<_, _>>()?,
			}

        } else if without_quotes(&field.ty) {
	        quote! {
	            #field_name: serde_json::from_str(&format!("\"{}\"", params_map.remove(#field_name_str)
	                .ok_or_else(|| MissingField(#field_name_str.to_string()))?))
	                .map_err(|e| ParseFieldError(#field_name_str.to_string(), e.to_string()))?,
	        }

		// Handle other json-serializable types
        } else {
        	quote! {
         		#field_name: serde_json::from_str(&params_map.remove(#field_name_str)
		            .ok_or_else(|| MissingField(#field_name_str.to_string()))?)
		            .map_err(|e| ParseFieldError(#field_name_str.to_string(), e.to_string()))?,
            }
        })
     });

    let expanded = quote! {
        impl IpcSerialize for #name {
            fn ipc_serialize(&self) -> String {
                use IpcSerializeError::*;

                let mut params_map = std::collections::HashMap::new();
                #(#serialize_fields)*

                let mut subnet_data = String::new();
                subnet_data.push_str(#tag);

                // Convert to Vec, sort by key, then format
                let mut params: Vec<_> = params_map.into_iter().collect();
                params.sort_by(|(a, _), (b, _)| a.cmp(b));

                for (key, value) in params {
                    subnet_data.push_str(&format!("{}{}={}", crate::IPC_TAG_DELIMITER, key, value));
                }

                subnet_data
            }

            fn ipc_deserialize(s: &str) -> Result<Self, IpcSerializeError> {
                use IpcSerializeError::*;

                let mut params_map = std::collections::HashMap::new();
                let parts: Vec<&str> = s.split(crate::IPC_TAG_DELIMITER).collect();
                for part in parts.iter().skip(1) {
                    let kv: Vec<&str> = part.splitn(2, '=').collect();
                    if kv.len() == 2 {
                        params_map.insert(kv[0].to_string(), kv[1].to_string());
                    }
                }

                Ok(Self {
                    #(#deserialize_fields)*
                })
            }
        }
    };

    TokenStream::from(expanded)
}
