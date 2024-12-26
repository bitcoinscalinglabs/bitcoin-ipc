use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, Data, DeriveInput, Expr, Fields};

/// Derive macro for IPC serialization — a custom serialization format for IPC messages in
/// Bitcoin transactions.
///
/// Uses serde_json to serialize and deserialize individual fields.
/// Differrent rules for serialization can be defined for different types.
#[proc_macro_derive(IPCSerialize, attributes(tag))]
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
    let insert_fields = fields.iter().map(|field| {
        let field_name = &field.ident;
        let field_name_str = field_name.as_ref().unwrap().to_string();

        // Handle Vec<String> fields separately
        if &field.ty == &syn::parse_quote!(Vec<String>) {
            quote! {
                params_map.insert(#field_name_str, self.#field_name.join(","));
            }
        } else {
		    quote! {
		        params_map.insert(#field_name_str, serde_json::to_string(&self.#field_name).unwrap());
		    }
        }
    });

    let deserialize_fields = fields.iter().map(|field| {
         let field_name = &field.ident;
         let field_name_str = field_name.as_ref().unwrap().to_string();

         // Handle Vec<String> fields separately
         if &field.ty == &syn::parse_quote!(Vec<String>) {
            quote! {
                #field_name: params_map.remove(#field_name_str).unwrap_or_default().split(',').map(|s| s.to_string()).collect(),
            }
        } else {
	        quote! {
	            #field_name: serde_json::from_str(&params_map.remove(#field_name_str).unwrap_or_default()).unwrap_or_default(),
	        }
        }
     });

    let expanded = quote! {
        impl IPCSerialize for #name {
            fn ipc_serialize(&self) -> String {
                let mut params_map = std::collections::HashMap::new();
                #(#insert_fields)*

                let mut subnet_data = String::new();
                subnet_data.push_str(#tag);

                for (key, value) in &params_map {
                    subnet_data.push_str(&format!("{}{}={}", crate::IPC_TAG_DELIMITER, key, value));
                }

                subnet_data
            }

            fn ipc_deserialize(s: &str) -> Self {
                let mut params_map = std::collections::HashMap::new();
                let parts: Vec<&str> = s.split(crate::IPC_TAG_DELIMITER).collect();
                for part in parts.iter().skip(1) {
                    let kv: Vec<&str> = part.splitn(2, '=').collect();
                    if kv.len() == 2 {
                        params_map.insert(kv[0].to_string(), kv[1].to_string());
                    }
                }

                Self {
                    #(#deserialize_fields)*
                }
            }
        }
    };

    TokenStream::from(expanded)
}
